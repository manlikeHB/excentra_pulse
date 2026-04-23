use excentra_pulse::config::Config;
use std::collections::HashMap;

fn main() {
    let bot = Config::from_env();

    println!("bot: {:?}", bot);
}

pub struct BotState {
    pub access_token: String,
    pub bids: HashMap<String, u8>, // pair -> count
    pub asks: HashMap<String, u8>,
}
