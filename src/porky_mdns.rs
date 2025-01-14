#![no_std]
#![no_main]

use core::net::{Ipv4Addr, Ipv6Addr};
use cyw43::{Control, JoinAuth, JoinOptions};
use cyw43_pio::PioSpi;
use defmt::{error, info, unwrap, warn};
use defmt_rtt as _;
use edge_mdns::buf::VecBufAccess;
use edge_mdns::domain::base::Ttl;
use edge_mdns::host::{Host, Service, ServiceAnswers};
use edge_mdns::io::IPV4_DEFAULT_SOCKET;
use edge_mdns::{io, HostAnswersMdnsHandler};
use edge_nal::UdpSplit;
use edge_nal_embassy::{Udp, UdpBuffers};
use embassy_executor::Spawner;
use embassy_net::{Config, Runner, Stack, StackResources};
use embassy_rp::bind_interrupts;
use embassy_rp::clocks::RoscRng;
use embassy_rp::gpio::{Level, Output};
use embassy_rp::peripherals::{DMA_CH0, PIO0};
use embassy_rp::pio::InterruptHandler as PioInterruptHandler;
use embassy_rp::pio::Pio;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::signal::Signal;
use embassy_time::Timer;
use panic_probe as _;
use static_cell::StaticCell;
use rand::RngCore;

pub const TCP_MDNS_SERVICE_NAME: &str = "_pigg";
pub const TCP_MDNS_SERVICE_PROTOCOL: &str = "_tcp";
pub const TCP_MDNS_SERVICE_TYPE: &str = "_pigg._tcp.local.";

const SSID_NAME: &str = "MOVISTAR_8A9E";
const SSID_PASS: &str = "E68N8MA422GRQJQTPqjN";
const SSID_SECURITY: &str = "wpa2";
const STACK_RESOURCES_SOCKET_COUNT: usize = 5;

bind_interrupts!(struct Irqs {
    PIO0_IRQ_0 => PioInterruptHandler<PIO0>;
});

#[embassy_executor::task]
async fn wifi_task(
    runner: cyw43::Runner<'static, Output<'static>, PioSpi<'static, PIO0, 0, DMA_CH0>>,
) -> ! {
    runner.run().await
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, cyw43::NetDriver<'static>>) -> ! {
    runner.run().await
}

// Wait for the DHCP service to come up and for us to get an IP address
async fn wait_for_ip(stack: &Stack<'static>) -> Result<Ipv4Addr, &'static str> {
    info!("Waiting for an IP address");
    while !stack.is_config_up() {
        Timer::after_millis(100).await;
    }
    let if_config = stack.config_v4().ok_or("Could not get IP Config")?;
    Ok(if_config.address.address())
}

pub async fn join(
    control: &mut Control<'_>,
    stack: Stack<'static>,
) -> Result<Ipv4Addr, &'static str> {
    let mut attempt = 1;
    while attempt <= 3 {
        info!(
            "Attempt #{} to join wifi network: '{}' with security = '{}'",
            attempt, SSID_NAME, SSID_SECURITY
        );

        let mut join_options = JoinOptions::new(SSID_PASS.as_bytes());

        match SSID_SECURITY {
            "open" => join_options.auth = JoinAuth::Open,
            "wpa" => join_options.auth = JoinAuth::Wpa,
            "wpa2" => join_options.auth = JoinAuth::Wpa2,
            "wpa3" => join_options.auth = JoinAuth::Wpa3,
            _ => {
                error!("Security '{}' is not supported", SSID_SECURITY);
                return Err("Security of SsidSpec i snot supported");
            }
        };

        match control.join(SSID_NAME, join_options).await {
            Ok(_) => {
                info!("Joined Wi-Fi network: '{}'", SSID_NAME);
                return wait_for_ip(&stack).await;
            }
            Err(_) => {
                attempt += 1;
                warn!("Failed to join wifi, retrying");
            }
        }
    }

    Err("Failed to join Wifi after too many retries")
}

/// Initialize the cyw43 chip and start device_net
async fn start_net<'a>(
    spawner: Spawner,
    pin_23: embassy_rp::peripherals::PIN_23,
    spi: PioSpi<'static, PIO0, 0, DMA_CH0>,
) -> (Control<'a>, Stack<'static>) {
    let fw = include_bytes!("../assets/43439A0.bin");
    let clm = include_bytes!("../assets/43439A0_clm.bin");
    let pwr = Output::new(pin_23, Level::Low);

    static STATE: StaticCell<cyw43::State> = StaticCell::new();
    let state = STATE.init(cyw43::State::new());
    let (net_device, mut control, runner) = cyw43::new(state, pwr, spi, fw).await;
    spawner.spawn(wifi_task(runner)).unwrap();

    control.init(clm).await;
    control
        .set_power_management(cyw43::PowerManagementMode::PowerSave)
        .await;

    static RESOURCES: StaticCell<StackResources<STACK_RESOURCES_SOCKET_COUNT>> = StaticCell::new();
    let resources = RESOURCES.init(StackResources::new());

    let mut rng = RoscRng;
    let seed = rng.next_u64();

    let config = Config::dhcpv4(Default::default());

    // Init network stack
    let (stack, runner) = embassy_net::new(net_device, config, resources, seed);

    unwrap!(spawner.spawn(net_task(runner)));

    (control, stack)
}

/// mDNS responder embassy task
#[embassy_executor::task]
async fn mdns_responder(
    stack: Stack<'static>,
    ipv4: Ipv4Addr,
    port: u16,
    serial_number: &'static str,
    model: &'static str,
    service: &'static str,
    protocol: &'static str,
) {
    let udp_buffers: UdpBuffers<{ STACK_RESOURCES_SOCKET_COUNT }, 1500, 1500, 2> =
        UdpBuffers::new();
    let udp = Udp::new(stack, &udp_buffers);
    let bind = io::bind(&udp, IPV4_DEFAULT_SOCKET, Some(Ipv4Addr::UNSPECIFIED), None).await;

    match bind {
        Ok(mut socket) => {
            let (recv, send) = socket.split();

            let signal = Signal::new();

            let (recv_buf, send_buf) = (
                VecBufAccess::<NoopRawMutex, 1500>::new(),
                VecBufAccess::<NoopRawMutex, 1500>::new(),
            );

            let mdns = io::Mdns::<NoopRawMutex, _, _, _, _>::new(
                Some(Ipv4Addr::UNSPECIFIED),
                None,
                recv,
                send,
                recv_buf,
                send_buf,
                |buf| RoscRng.fill_bytes(buf),
                &signal,
            );

            // Host we are announcing from - not sure how important this is
            let host = Host {
                hostname: "host1",
                ipv4,
                ipv6: Ipv6Addr::UNSPECIFIED,
                ttl: Ttl::from_secs(60),
            };

            // The service we will be announcing over mDNS
            let service = Service {
                name: serial_number,
                priority: 1,
                weight: 5,
                service,
                protocol,
                port,
                service_subtypes: &[],
                txt_kvs: &[
                    ("Serial", serial_number),
                    ("Model", model),
                    ("AppName", env!("CARGO_BIN_NAME")),
                    ("AppVersion", env!("CARGO_PKG_VERSION")),
                ],
            };

            info!("Starting mDNS responder");
            let ha = HostAnswersMdnsHandler::new(ServiceAnswers::new(&host, &service));
            if (mdns.run(ha).await).is_err() {
                error!("Could not run mdns responder");
            }

            info!("Exiting mDNS responder");
        }
        Err(_) => {
            error!("Could not bind to io Socket in mDNS");
        }
    }
}

#[embassy_executor::main]
async fn main(spawner: Spawner) {
    // Get the RPi Pico Peripherals - a number of the PINS are available for GPIO (they are
    // passed to AvailablePins) while others are reserved for internal use and not available for
    // GPIO
    let peripherals = embassy_rp::init(Default::default());
    // PIN_25 - OP wireless SPI CS - when high also enables GPIO29 ADC pin to read VSYS
    let cs = Output::new(peripherals.PIN_25, Level::High);
    let mut pio = Pio::new(peripherals.PIO0, Irqs);

    // Initialize the cyw43 and start the network
    let spi = PioSpi::new(
        &mut pio.common,
        pio.sm0,
        pio.irq0,
        cs,
        // PIN_24 - OP/IP wireless SPI data/IRQ
        peripherals.PIN_24,
        // PIN_29 - OP/IP wireless SPI CLK/ADC mode (ADC3) to measure VSYS/3
        peripherals.PIN_29,
        peripherals.DMA_CH0,
    );
    // PIN_23 - OP wireless power on signal
    let (mut control, wifi_stack) = start_net(spawner, peripherals.PIN_23, spi).await;

    let _ = control.add_multicast_address([0x01, 0x00, 0x5e, 0x00, 0x00, 0xfb]).await;

    let ip = join(&mut control, wifi_stack).await.unwrap();
    info!("Assigned IP: {}", ip);

    let _ = spawner.spawn(mdns_responder(
        wifi_stack,
        ip,
        1234,
        "123456789",
        "Pi Pico W",
        TCP_MDNS_SERVICE_NAME,
        TCP_MDNS_SERVICE_PROTOCOL,
    ));
}
