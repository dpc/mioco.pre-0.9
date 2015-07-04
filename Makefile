PKG_NAME=mioco
DOCS_DEFAULT_MODULE=mioco
DEFAULT_TARGET=build

default: $(DEFAULT_TARGET)

.PHONY: run test build doc clean release rrun rtest
run test build doc clean:
	cargo $@

release:
	cargo build --release

rrun:
	cargo run --release

rtest:
	cargo test --release

publishdoc: doc
	echo '<meta http-equiv="refresh" content="0;url='${DOCS_DEFAULT_MODULE}'/index.html">' > target/doc/index.html
	ghp-import -n target/doc
	git push origin gh-pages

.PHONY: docview
docview: doc
	xdg-open target/doc/$(PKG_NAME)/index.html


.PHONY: echo
	cargo run --example echo --release
