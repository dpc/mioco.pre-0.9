PKG_NAME=$(shell grep name Cargo.toml | head -n 1 | awk -F \" '{print $$2}')
DOCS_DEFAULT_MODULE=$(subst -,_,$(PKG_NAME))
$(info Building $(PKG_NAME))
ifeq (, $(shell which cargo-check 2> /dev/null))
DEFAULT_TARGET=build
else
DEFAULT_TARGET=check
endif

default: $(DEFAULT_TARGET)

ALL_TARGETS += build $(EXAMPLES) test doc
ifneq ($(RELEASE),)
$(info RELEASE BUILD: $(PKG_NAME))
CARGO_FLAGS += --release
ALL_TARGETS += bench
else
$(info DEBUG BUILD: $(PKG_NAME); use `RELEASE=true make [args]` for release build)
endif

EXAMPLES = $(shell cd examples 2>/dev/null && ls *.rs 2>/dev/null | sed -e 's/.rs$$//g' )

all: $(ALL_TARGETS)

.PHONY: run test build doc clean clippy
run test build clean:
	cargo $@ $(CARGO_FLAGS)

check:
	$(info Running check; use `make build` to actually build)
	cargo $@ $(CARGO_FLAGS)

clippy:
	cargo build --features clippy

.PHONY: bench
bench:
	cargo $@ $(filter-out --release,$(CARGO_FLAGS))

.PHONY: travistest
travistest:
	for i in `seq 10`; do cargo test $(CARGO_FLAGS) || exit 1 ; done

.PHONY: longtest
longtest:
	@echo "Running longtest. Press Ctrl+C to stop at any time"
	@sleep 2
	@i=0; while i=$$((i + 1)) && echo "Iteration $$i" && cargo test $(CARGO_FLAGS) ; do :; done

.PHONY: $(EXAMPLES)
$(EXAMPLES):
	cargo build --example $@ $(CARGO_FLAGS)

.PHONY: doc
doc: FORCE
	rm -rf target/doc
	cargo doc

.PHONY: publishdoc
publishdoc: doc
	echo '<meta http-equiv="refresh" content="0;url='${DOCS_DEFAULT_MODULE}'/index.html">' > target/doc/index.html
	ghp-import -n target/doc
	git push -f origin gh-pages

.PHONY: docview
docview: doc
	xdg-open target/doc/$(PKG_NAME)/index.html

.PHONY: FORCE
FORCE:
