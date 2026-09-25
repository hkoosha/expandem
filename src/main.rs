use expandem::parse_options;

fn main() {
    // env_logger::Builder::new()
    //     .parse_write_style("RUST_LOG_STYLE")
    //     .filter_level(log::LevelFilter::Trace)
    //     .init();

    env_logger::init();

    let mut args = std::env::args();
    let bin = args.next();

    let options = match parse_options(bin, args) {
        Ok(it) => it,
        Err((stderr, exit_code)) => {
            eprintln!("{stderr}");
            std::process::exit(exit_code);
        }
    };

    match expandem::expand(options) {
        Ok(it) => print!("{}", it),
        Err(err) => {
            eprintln!("{err}");
            std::process::exit(1);
        }
    };
}
