.PHONY: help check preview

help:
	@echo "OTDEL — available targets:"
	@echo "  make check    - run repository hygiene checks (scripts/check.mjs, Node >= 22)"
	@echo "  make preview  - serve design/ ONLY at http://127.0.0.1:4173 (Python http.server)"

check:
	node scripts/check.mjs

# Serves only the design/ directory (static prototype: index.html, prototype.js/css,
# assets/). Binds to 127.0.0.1 (localhost only, not 0.0.0.0) and uses --directory so
# the server root is design/, not the repository root — nothing outside design/ is
# reachable through this server.
preview:
	python3 -m http.server 4173 --bind 127.0.0.1 --directory design
