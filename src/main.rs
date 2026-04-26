use excentra_pulse::{bot::Bot, config::Config};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().init();
    let config = Config::from_env();
    let mut bot = Bot::new(config);
    bot.run().await;
}
