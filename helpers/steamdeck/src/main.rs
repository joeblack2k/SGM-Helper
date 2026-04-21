fn main() {
    if let Err(err) = sgm_steamdeck_helper::run() {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}
