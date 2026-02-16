use blockcell_channels::ChannelManager;
use blockcell_core::{Config, Paths};
use tokio::sync::mpsc;

pub async fn status() -> anyhow::Result<()> {
    let paths = Paths::new();
    let config = Config::load_or_default(&paths)?;

    // Create a dummy channel for ChannelManager (we just need status)
    let (tx, _rx) = mpsc::channel(1);
    let manager = ChannelManager::new(config, paths, tx);

    println!("Channel Status");
    println!("==============");
    println!();

    for (name, enabled, info) in manager.get_status() {
        let status = if enabled { "✓" } else { "✗" };
        println!("{} {:<10} {}", status, name, info);
    }

    Ok(())
}

pub async fn login(channel: &str) -> anyhow::Result<()> {
    match channel {
        "whatsapp" => {
            println!("WhatsApp login:");
            println!("  1. Ensure the WhatsApp bridge is running");
            println!("  2. The bridge will display a QR code");
            println!("  3. Scan the QR code with WhatsApp on your phone");
            println!();
            println!("To start the bridge manually:");
            println!("  cd ~/.blockcell/bridge && npm start");
        }
        _ => {
            println!("Login not supported for channel: {}", channel);
            println!("Supported channels: whatsapp");
        }
    }

    Ok(())
}
