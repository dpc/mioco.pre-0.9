include Makefile.defs

default: $(DEFAULT_TARGET)

.PHONY: run test build doc clean release rrun rtest
run test build doc clean:
	cargo $@

simple:
	cargo run

release:
	cargo build --release

rrun:
	cargo run --release

rtest:
	cargo test --release

echo: rtest
	./target/release/examples/echo

.PHONY: docview
docview: doc
	xdg-open target/doc/$(PKG_NAME)/index.html
