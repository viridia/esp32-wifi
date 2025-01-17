#![no_std]
#![no_main]

use core::str::FromStr;

use embassy_executor::Spawner;
use embassy_net::{Stack, StackResources};
use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_hal::prelude::*;
use esp_println::println;
use esp_wifi::{
    wifi::{
        AuthMethod, ClientConfiguration, Configuration, WifiController, WifiDevice, WifiError,
        WifiEvent, WifiStaDevice, WifiState,
    },
    EspWifiController,
};
use heapless::String;
use log::{info, warn};

extern crate alloc;

// When you are okay with using a nightly compiler it's better to use https://docs.rs/static_cell/2.1.0/static_cell/macro.make_static.html
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

#[main]
async fn main(spawner: Spawner) {
    let peripherals = esp_hal::init({
        let mut config = esp_hal::Config::default();
        config.cpu_clock = CpuClock::max();
        config
    });

    esp_alloc::heap_allocator!(72 * 1024);

    esp_println::logger::init_logger_from_env();

    let timer0 = esp_hal::timer::systimer::SystemTimer::new(peripherals.SYSTIMER)
        .split::<esp_hal::timer::systimer::Target>();
    esp_hal_embassy::init(timer0.alarm0);

    info!("Embassy initialized!");

    let timer1 = esp_hal::timer::timg::TimerGroup::new(peripherals.TIMG0);
    let wifi_init = &*mk_static!(
        EspWifiController<'static>,
        esp_wifi::init(
            timer1.timer0,
            esp_hal::rng::Rng::new(peripherals.RNG),
            peripherals.RADIO_CLK,
        )
        .unwrap()
    );

    let wifi = peripherals.WIFI;
    let (wifi_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(wifi_init, wifi, WifiStaDevice).unwrap();

    controller.start().unwrap();

    info!("Waiting for WiFi initialization...");
    loop {
        match controller.is_started() {
            Ok(started) => {
                if started {
                    break;
                }
            }
            Err(err) => {
                info!("Error: {:?}", err);
            }
        }
        Timer::after(Duration::from_millis(100)).await;
    }
    info!("WiFi initialized!");

    info!("\nStarting WiFi scan...");
    let cc = get_access_point(&mut controller).await;
    if let Err(e) = cc {
        info!("Error: {:?}", e);
        return;
    }
    let cc = cc.unwrap();

    info!("Starting wifi on {}", cc.ssid);
    let client_config = Configuration::Client(cc);
    // controller.stop_async().await.unwrap();
    controller.set_configuration(&client_config).unwrap();
    // controller.start_async().await.unwrap();

    let config = embassy_net::Config::dhcpv4(Default::default());

    let seed = 1234; // very random, very secure seed

    // Init network stack
    let stack = &*mk_static!(
        Stack<WifiDevice<'_, WifiStaDevice>>,
        Stack::new(
            wifi_interface,
            config,
            mk_static!(StackResources<3>, StackResources::<3>::new()),
            seed
        )
    );

    spawner.spawn(connection(controller)).ok();
    spawner.spawn(net_task(stack)).ok();

    // Wait until wifi connected
    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    println!("Waiting to get IP address...");
    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address); //dhcp IP address
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }
}

#[derive(Debug)]
pub enum ScanError {
    NoPublicNetworks,
    WifiError(WifiError),
}

async fn get_access_point<'w>(
    controller: &mut WifiController<'w>,
) -> Result<ClientConfiguration, ScanError> {
    // If we specified an AP via environment variables, use that.
    #[allow(clippy::const_is_empty)]
    if !SSID.is_empty() {
        let cc = ClientConfiguration {
            ssid: String::from_str(SSID).unwrap(),
            auth_method: if PASSWORD.is_empty() {
                AuthMethod::None
            } else {
                AuthMethod::WPA2Personal
            },
            password: String::from_str(PASSWORD).unwrap(),
            ..Default::default()
        };
        return Ok(cc);
    }

    // Otherwise, scan for an open network.
    for _i in 0..5 {
        match controller.scan_n_async::<20>().await {
            Ok((mut scan_result, _)) => {
                scan_result.sort_by(|a, b| b.signal_strength.cmp(&a.signal_strength));
                if let Some(ap) = scan_result
                    .iter()
                    .find(|ap| ap.auth_method == Some(AuthMethod::None))
                {
                    info!(
                        "Found Open Wifi: SSID: {}, Channel: {}, RSSI: {}dBm",
                        ap.ssid, ap.channel, ap.signal_strength
                    );
                    return Ok(ClientConfiguration {
                        ssid: ap.ssid.clone(),
                        channel: Some(ap.channel),
                        auth_method: AuthMethod::None,
                        ..Default::default()
                    });
                }
                warn!("No open Wifi, found {} networks:", scan_result.len());
                for ap in scan_result {
                    println!(
                        "SSID: {}, Channel: {}, RSSI: {}dBm, Auth: {:?}",
                        ap.ssid, ap.channel, ap.signal_strength, ap.auth_method
                    );
                }
            }
            Err(e) => {
                return Err(ScanError::WifiError(e));
            }
        }
        Timer::after(Duration::from_secs(5)).await;
    }

    Err(ScanError::NoPublicNetworks)
}

#[embassy_executor::task]
async fn connection(mut controller: WifiController<'static>) {
    info!("Start connection task");
    // println!("Device capabilities: {:?}", controller.capabilities());
    loop {
        if esp_wifi::wifi::wifi_state() == WifiState::StaConnected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            Timer::after(Duration::from_millis(5000)).await
        }

        // Should have already been started earlier
        match controller.is_started() {
            Ok(started) if !started => {
                info!("Restarting wifi");
                // let client_config = Configuration::Client(ClientConfiguration {
                //     ssid: ap.ssid.clone(),
                //     auth_method: ap.auth_method,
                //     password: ap.password.clone(),
                //     ..Default::default()
                // });
                // controller.set_configuration(&client_config).unwrap();
                controller.start_async().await.unwrap();
                info!("Wifi started!");
            }
            Ok(_) => {}
            Err(err) => {
                log::error!("Error: {:?}", err);
                Timer::after(Duration::from_millis(5000)).await;
                continue;
            }
        }

        info!("About to connect...");
        match controller.connect_async().await {
            Ok(_) => info!("Wifi connected!"),
            Err(e) => {
                log::error!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}

#[embassy_executor::task]
async fn net_task(stack: &'static Stack<WifiDevice<'static, WifiStaDevice>>) {
    stack.run().await
}
