//! `rax`, the RAX command line.

fn main() {
    let code = rax_cli::run(std::env::args().skip(1).collect());
    std::process::exit(code);
}
