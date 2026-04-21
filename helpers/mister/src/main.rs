fn main() {
    if let Err(err) = sgm_mister_helper::run() {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}
