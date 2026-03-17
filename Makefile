release:
	cargo build --release && \
	cp target/release/blockcell ~/.local/bin/ && \
	blockcell --version


reload:
	cp -r skills/* ~/.blockcell/workspace/skills/ || true
	cargo run -p blockcell -- skills reload && \
	blockcell --version