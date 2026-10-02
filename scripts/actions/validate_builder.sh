cargo test -p zimz-core --test synthetic -- --ignored write_synthetic

uv run scripts/validate_synthetic.py
