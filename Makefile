PKG_NAME=mioco
DOCS_DEFAULT_MODULE=mioco
DEFAULT_TARGET=build

default: $(DEFAULT_TARGET)

ifneq ($(RELEASE),)
CARGO_FLAGS += --release
$(info RELEASE BUILD)
else
$(info DEBUG BUILD; use `RELEASE=true make [args]` for release build)
endif

EXAMPLES = echo echoplus mailbox

all: build $(EXAMPLES) test

.PHONY: run test build doc clean
run test build clean:
	cargo $@ $(CARGO_FLAGS)

.PHONY: longtest
longtest:
	for i in `seq 100`; do cargo test $(CARGO_FLAGS) || exit 1 ; done

.PHONY: $(EXAMPLES)
$(EXAMPLES):
	cargo build --example $@ $(CARGO_FLAGS)

doc: FORCE
	cp src/lib.rs src/lib.rs.orig
	sed -i -e '/\/\/ MAKE_DOC_REPLACEME/{ r examples/echo.rs' -e 'd  }' src/lib.rs
	-cargo doc
	mv src/lib.rs.orig src/lib.rs

publishdoc: doc
	echo '<meta http-equiv="refresh" content="0;url='${DOCS_DEFAULT_MODULE}'/index.html">' > target/doc/index.html
	ghp-import -n target/doc
	git push -f origin gh-pages


.PHONY: docview
docview: doc
	xdg-open target/doc/$(PKG_NAME)/index.html

.PHONY: FORCE
FORCE:
