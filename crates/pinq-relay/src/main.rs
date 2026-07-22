//! `pinq-relay` — run a Pinq sync relay.
//!
//!   pinq-relay [--bind 0.0.0.0:47820] [--data ./pinq-relay-data] [--keep-days 30]

use pinq_relay::{serve, Relay, RelayConfig};
use std::net::TcpListener;
use std::sync::Arc;

fn main() {
    let mut bind = "0.0.0.0:47820".to_string();
    let mut data = std::path::PathBuf::from("./pinq-relay-data");
    let mut keep_days = 30u64;

    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--bind" => bind = args.next().unwrap_or(bind),
            "--data" => data = args.next().map(Into::into).unwrap_or(data),
            "--keep-days" => {
                keep_days = args.next().and_then(|v| v.parse().ok()).unwrap_or(keep_days)
            }
            "--help" | "-h" => {
                println!("pinq-relay [--bind ADDR:PORT] [--data DIR] [--keep-days N]");
                println!("  Store-and-forward relay for Pinq sync. Envelopes are end-to-end");
                println!("  encrypted by the devices; this server only ever sees ciphertext.");
                return;
            }
            other => {
                eprintln!("unknown argument: {other} (try --help)");
                std::process::exit(2);
            }
        }
    }

    if let Err(e) = std::fs::create_dir_all(&data) {
        eprintln!("cannot create data dir {}: {e}", data.display());
        std::process::exit(1);
    }
    let listener = match TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("cannot bind {bind}: {e}");
            std::process::exit(1);
        }
    };
    let relay = match Relay::new(RelayConfig { data_dir: data.clone(), keep_days }) {
        Ok(r) => Arc::new(r),
        Err(e) => {
            eprintln!("cannot initialize relay in {}: {e}", data.display());
            std::process::exit(1);
        }
    };
    println!(
        "pinq-relay listening on {bind}, storing envelopes in {} (retention: {})",
        data.display(),
        if keep_days == 0 { "forever".into() } else { format!("{keep_days} days") },
    );
    println!("relay id: {}", relay.public_key_hex());
    println!("(devices pin this identity on first contact; to pre-pin, enter the");
    println!(" relay in Pinq as  host:port#{}  )", &relay.public_key_hex()[..16]);
    serve(listener, relay);
}
