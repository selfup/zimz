Fuzz targets for zimz-core (requires nightly + `cargo install cargo-fuzz`):

    cargo +nightly fuzz run header
    cargo +nightly fuzz run dirent
    cargo +nightly fuzz run archive -- -max_len=65536

Seed the `archive` corpus with `fixtures/zim-testing-suite/data/*/small.zim`.
