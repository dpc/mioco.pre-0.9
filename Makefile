PKG_NAME=mioco
DOCS_DEFAULT_MODULE=mioco
DEFAULT_TARGET=build

default: $(DEFAULT_TARGET)

.PHONY: run test build doc clean release rrun rtest
run test build clean:
	cargo $@

doc: FORCE
	cp src/lib.rs src/lib.rs.orig
	sed -i -e '/\/\/ MAKE_DOC_REPLACEME/{ r examples/echo.rs' -e 'd  }' src/lib.rs
	-cargo doc
	mv src/lib.rs.orig src/lib.rs

release:
	cargo build --release

rrun:
	cargo run --release

rtest:
	cargo test --release

publishdoc: doc
	echo '<meta http-equiv="refresh" content="0;url='${DOCS_DEFAULT_MODULE}'/index.html">' > target/doc/index.html
	ghp-import -n target/doc
	git push -f origin gh-pages

.PHONY: docview
docview: doc
	xdg-open target/doc/$(PKG_NAME)/index.html

.PHONY: echo
echo:
	cargo run --example echo --release

decho:
	cargo run --example echo

.PHONY: FORCE
FORCE:

