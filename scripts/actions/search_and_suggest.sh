mkdir -p target/parity

f=fixtures/zim-testing-suite/data/nons/wikipedia_en_climate_change_mini_2024-06.zim

uv run scripts/search_parity.py "$f" -o target/parity/search-climate.json

ZIMZ_SEARCH_PARITY=target/parity/search-climate.json cargo test -p zimz-glass --test search_parity

uv run scripts/suggest_parity.py "$f" -o target/parity/suggest-climate.json

ZIMZ_SUGGEST_PARITY=target/parity/suggest-climate.json cargo test -p zimz-glass --test suggest_parity
