#![no_std]
#![no_main]
use embassy_executor::Spawner;
use embassy_net::{tcp::TcpSocket, Ipv4Address, Runner, Stack, StackResources};
use embassy_time::{Duration, Timer};
use esp32_wifi::wifi::{connection, get_access_point};
use esp_alloc as _;
use esp_backtrace as _;
use esp_hal::{clock::CpuClock, rng::Rng, timer::timg::TimerGroup};
use esp_println::println;
use esp_wifi::{
    init,
    wifi::{Configuration, WifiDevice, WifiStaDevice},
    EspWifiController,
};
use log::info;

// When you are okay with using a nightly compiler it's better to use https://docs.rs/static_cell/2.1.0/static_cell/macro.make_static.html
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write(($val));
        x
    }};
}

// const SSID: &str = env!("SSID");
// const PASSWORD: &str = env!("PASSWORD");

#[esp_hal_embassy::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let peripherals = esp_hal::init({
        let mut config = esp_hal::Config::default();
        config.cpu_clock = CpuClock::max();
        config
    });

    esp_alloc::heap_allocator!(72 * 1024);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let mut rng = Rng::new(peripherals.RNG);

    let init = &*mk_static!(
        EspWifiController<'static>,
        init(timg0.timer0, rng, peripherals.RADIO_CLK).unwrap()
    );

    use esp_hal::timer::systimer::SystemTimer;
    // let systimer =
    let systimer = SystemTimer::new(peripherals.SYSTIMER);
    esp_hal_embassy::init(systimer.alarm0);

    let (wifi_interface, mut controller) =
        esp_wifi::wifi::new_with_mode(init, peripherals.WIFI, WifiStaDevice).unwrap();

    let client_config = get_access_point(&mut controller).await.unwrap();

    // Init network stack
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    // let config = embassy_net::Config::dhcpv4(Default::default());
    let net_config = embassy_net::Config::dhcpv4(Default::default());
    // let stack = &*mk_static!(
    //     Stack<WifiDevice<'_, WifiStaDevice>>,
    //     Stack::new(
    //         wifi_interface,
    //         net_config,
    //         mk_static!(StackResources<3>, StackResources::<3>::new()),
    //         seed
    //     )
    // );
    let (stack, runner) = embassy_net::new(
        wifi_interface,
        net_config,
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );

    let config = Configuration::Client(client_config);
    info!("Setting wifi configuration");
    controller.set_configuration(&config).unwrap();
    info!("Spawning connection task");
    spawner.spawn(connection(controller, config)).ok();
    info!("Spawning net task");
    spawner.spawn(net_task(runner)).ok();

    let mut rx_buffer = [0; 4096];
    let mut tx_buffer = [0; 4096];

    loop {
        if stack.is_link_up() {
            println!("Stack link is up");
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    loop {
        if let Some(config) = stack.config_v4() {
            println!("Got IP: {}", config.address);
            break;
        }
        println!("Waiting to get IP address...");
        Timer::after(Duration::from_millis(500)).await;
    }

    loop {
        Timer::after(Duration::from_millis(1_000)).await;

        let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);

        socket.set_timeout(Some(embassy_time::Duration::from_secs(10)));

        let remote_endpoint = (Ipv4Address::new(142, 250, 185, 115), 80);
        println!("connecting...");
        let r = socket.connect(remote_endpoint).await;
        if let Err(e) = r {
            println!("connect error: {:?}", e);
            continue;
        }
        println!("connected!");
        let mut buf = [0; 1024];
        loop {
            use embedded_io_async::Write;
            let r = socket
                .write_all(b"GET / HTTP/1.0\r\nHost: www.mobile-j.de\r\n\r\n")
                .await;
            if let Err(e) = r {
                println!("write error: {:?}", e);
                break;
            }
            let n = match socket.read(&mut buf).await {
                Ok(0) => {
                    println!("read EOF");
                    break;
                }
                Ok(n) => n,
                Err(e) => {
                    println!("read error: {:?}", e);
                    break;
                }
            };
            println!("{}", core::str::from_utf8(&buf[..n]).unwrap());
        }
        Timer::after(Duration::from_millis(3000)).await;
    }
}

#[embassy_executor::task]
async fn net_task(mut runner: Runner<'static, WifiDevice<'static, WifiStaDevice>>) {
    runner.run().await
}
