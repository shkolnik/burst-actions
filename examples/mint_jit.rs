// cargo run --example mint_jit -- owner/repo runner-name
// Prints the encoded JIT config to stdout (and nothing else) for manual gate launches.
fn main() {
    let mut args = std::env::args().skip(1);
    let repo = burst::schema::RepoId::parse(&args.next().expect("owner/repo")).unwrap();
    let name = args.next().expect("runner-name");
    let client = burst::github::Client::new(burst::github::token_from_env().unwrap());
    println!("{}", client.mint_jit_config(&repo, &name).unwrap());
}
