mkdir -p target/parity

for f in nons/wikipedia_en_climate_change_mini_2024-06 withns/wikibooks_be_all_nopic_2017-02 noTitleListingV0/small; do
    uv run scripts/parity.py "fixtures/zim-testing-suite/data/$f.zim" --count 500 --seed 1 -o "target/parity/$(basename "$f").json"
    
    ZIMZ_PARITY_MANIFEST="target/parity/$(basename "$f").json" cargo test -p zimz-core --test parity
done
