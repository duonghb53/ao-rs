ROOT := $(shell pwd)
UI_DIR := crates/ao-desktop/ui
DASHBOARD_DIST := crates/ao-dashboard/ui-dist

.PHONY: all build ui clean-dist install help

all: install

help:
	@echo "Targets:"
	@echo "  make ui         - build desktop UI"
	@echo "  make clean-dist - remove dashboard ui-dist"
	@echo "  make install    - full build + cargo install ao-cli (default)"

ui:
	cd $(UI_DIR) && npm run build

clean-dist:
	rm -rf $(DASHBOARD_DIST)

install: ui clean-dist
	cargo install --path crates/ao-cli --locked --force
