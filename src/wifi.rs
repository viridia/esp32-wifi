use core::str::FromStr;

use embassy_time::{Duration, Timer};
use esp_backtrace as _;
use esp_println::println;
use esp_wifi::wifi::{
    AuthMethod, ClientConfiguration, Configuration, WifiController, WifiError, WifiEvent, WifiState,
};
use heapless::String;
use log::{info, warn};

const SSID: &str = env!("SSID");
const PASSWORD: &str = env!("PASSWORD");

#[derive(Debug)]
pub enum ScanError {
    NoPublicNetworks,
    WifiError(WifiError),
}

pub async fn get_access_point<'w>(
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
        println!("Using specified AP: {}", cc.ssid);
        return Ok(cc);
    }

    println!("Starting wifi for scan");
    controller.start_async().await.unwrap();

    info!("Initializing WiFi for scan...");
    loop {
        match controller.is_started() {
            Ok(started) => {
                if started {
                    break;
                }
            }
            Err(err) => {
                return Err(ScanError::WifiError(err));
            }
        }
        Timer::after(Duration::from_millis(100)).await;
    }
    info!("WiFi initialized!");

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

                    // controller.disconnect().unwrap();
                    controller.stop_async().await.unwrap();
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
pub async fn connection(mut controller: WifiController<'static>, client_config: Configuration) {
    println!("start connection task");
    println!("Device capabilities: {:?}", controller.capabilities());
    loop {
        if esp_wifi::wifi::wifi_state() == WifiState::StaConnected {
            // wait until we're no longer connected
            controller.wait_for_event(WifiEvent::StaDisconnected).await;
            println!("Wifi got disconnected, re-connecting...");
            Timer::after(Duration::from_millis(5000)).await
        }

        if !matches!(controller.is_started(), Ok(true)) {
            controller.set_configuration(&client_config).unwrap();
            println!("Starting wifi");
            controller.start_async().await.unwrap();
            println!("Wifi started!");
        }

        println!("About to connect...");
        match controller.connect_async().await {
            Ok(_) => println!("Wifi connected!"),
            Err(e) => {
                println!("Failed to connect to wifi: {e:?}");
                Timer::after(Duration::from_millis(5000)).await
            }
        }
    }
}
