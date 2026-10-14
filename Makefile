# Titan integration template — developer entry points.
#
#   make build-program   build the route program as SBPF v3
#   make check-structure lib tests + scorecard assertion + offline suite + enum parity
#   make test-example   reference (Raydium) suite — must be green / skipped
#   make test-venue      YOUR venue's suite (red until you implement YourVenue)
#   make scorecard       print the integration scorecard only
#   make dump-programs   fetch the on-chain program binaries the sim tests load
#
# Each phase reports one of:
#   ok       ran and passed
#   skipped  could not run — missing SOLANA_RPC_URL (and/or program dumps / a
#            built on-chain program); nothing was actually exercised
#   red      ran and failed (your venue isn't implemented / is wrong)
#   FAILED   a check that must always pass broke (the reference is broken)
#
# To actually run the RPC-gated tests:
#   export SOLANA_RPC_URL=https://...   &&   make build-program   &&   make dump-programs
#   make test-example
#   make test-venue

RAYDIUM := 675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8
SPL_CALC_1 := sspUE1vrh7xRoXxGsg7vR1zde2WdGtJRbyK9uRumBDy
SPL_CALC_2 := ssmbu3KZxgonUtjEMCKspZzxvUQCxAFnyh1rcHUeEDo
SENDFUN_LAUNCHPAD := 5R1uFyEE4oxqkm7hJDqHq6gXaLVF9dxeF3LF4yz1sfLP
SENDFUN_DEX := 84qj5FPZZdXkQy8mfowyg6RBZ3XKuTds6XS4ZYT1sfDX
PROGRAMS := $(RAYDIUM) $(SPL_CALC_1) $(SPL_CALC_2) $(SENDFUN_LAUNCHPAD) $(SENDFUN_DEX)

# One off-chain suite and one route suite per send.fun market, routed as a
# registered partner. `sendfun_hostile` adds a curve and a pool of hostile.xyz's
# platform. `sendfun_default_partner` runs every market again as the default
# partner.
VENUE_TESTS := sendfun_curve_wsol sendfun_curve_fee sendfun_dex_pool sendfun_hostile sendfun_default_partner
VENUE_ROUTE_TESTS := $(addsuffix _route,$(VENUE_TESTS))

DUMP_URL := $(if $(SOLANA_RPC_URL),$(SOLANA_RPC_URL),m)

PROGRAM := --manifest-path program-template/Cargo.toml
PLATFORM_TOOLS := v1.57
RELEASE_PROFILE := --release
# Used only for the construction allocation guard. Quote-speed runs in release.
ASSERT_PROFILE := --profile release-debug
SCORECARD = cargo test --quiet $(RELEASE_PROFILE) --test scorecard -- --nocapture 2>/dev/null | sed -n '/^====/,/^====/p'

.PHONY: build-program check-structure test-example test-venue scorecard dump-programs \
        _unit-phase _example-sim-phase _venue-phase

# --- always-on checks (no RPC): unit tests, scorecard assertion, offline suite, enum parity ---
# The offline suite runs under ASSERT_PROFILE, which keeps its allocation checks.
_unit-phase:
	@mkdir -p target
	@printf '\n================ Titan structural checks ====================\n\n'
	@printf '  %-24s  %-8s  %s\n' 'Check' 'Status' 'Detail'
	@printf '  %-24s  %-8s  %s\n' '------------------------' '--------' '----------------------------------------'
	@log=target/log-unit.txt; \
		if cargo test --quiet $(RELEASE_PROFILE) --lib --test scorecard >$$log 2>&1 \
			&& cargo test --quiet $(ASSERT_PROFILE) --test sendfun_offline >>$$log 2>&1 \
			&& cargo test --quiet $(PROGRAM) --release --lib --test venue_parity >>$$log 2>&1; \
		then st=ok; dt='lib tests + scorecard + offline suite + enum parity'; \
		else st=FAILED; dt='see log below'; fi; \
		printf '  %-24s  %-8s  %s\n' 'Unit + structure' "$$st" "$$dt"; \
		if [ $$st = FAILED ]; then echo; cat $$log; exit 1; fi

# --- reference (Raydium) simulations: need RPC + program dumps -----------------
# These MUST be green when run, hence FAILED (not red) and abort on failure.
_example-sim-phase:
	@mkdir -p target
	@printf '\n================ Titan example checks =======================\n\n'
	@printf '  %-24s  %-8s  %s\n' 'Check' 'Status' 'Detail'
	@printf '  %-24s  %-8s  %s\n' '------------------------' '--------' '----------------------------------------'
	@log=target/log-ex-off.txt; \
		cargo test --quiet $(RELEASE_PROFILE) --test example -- --skip construction --nocapture >$$log 2>&1; rc1=$$?; \
		cargo test --quiet $(ASSERT_PROFILE) --test example -- construction --nocapture >>$$log 2>&1; rc2=$$?; \
		cargo test --quiet $(RELEASE_PROFILE) --test venue_creation -- --nocapture >>$$log 2>&1; rc3=$$?; \
		if [ $$rc1 -ne 0 ] || [ $$rc2 -ne 0 ] || [ $$rc3 -ne 0 ]; then st=FAILED; dt='see log below'; \
		elif grep -q 'SKIP' $$log; then st=skipped; dt='set SOLANA_RPC_URL'; \
		else st=ok; dt='example suite passed'; fi; \
		printf '  %-24s  %-8s  %s\n' 'Off-chain' "$$st" "$$dt"; \
		if [ $$st = FAILED ]; then echo; cat $$log; exit 1; fi
	@log=target/log-ex-prog.txt; \
		cargo test --quiet $(PROGRAM) --release --test example_route -- --nocapture >$$log 2>&1; rc=$$?; \
		if [ $$rc -ne 0 ]; then st=FAILED; dt='see log below'; \
		elif grep -q 'SKIP' $$log; then st=skipped; dt='needs fresh build-program + RPC + dumps'; \
		else st=ok; dt='route suite passed'; fi; \
		printf '  %-24s  %-8s  %s\n' 'On-chain program' "$$st" "$$dt"; \
		if [ $$st = FAILED ]; then echo; cat $$log; exit 1; fi

# --- your venue: red until implemented; skips without RPC ----------------------
# A venue failure is expected work-in-progress, so it reports "red" and does NOT
# abort — the scorecard still prints.
_venue-phase:
	@mkdir -p target
	@printf '\n================ Titan venue checks =========================\n\n'
	@printf '  %-24s  %-8s  %s\n' 'Check' 'Status' 'Detail'
	@printf '  %-24s  %-8s  %s\n' '------------------------' '--------' '----------------------------------------'
	@log=target/log-venue-off.txt; \
		cargo test --quiet $(RELEASE_PROFILE) $(addprefix --test ,$(VENUE_TESTS)) -- --skip construction --nocapture >$$log 2>&1; rc1=$$?; \
		cargo test --quiet $(ASSERT_PROFILE) $(addprefix --test ,$(VENUE_TESTS)) -- construction --nocapture >>$$log 2>&1; rc2=$$?; \
		cargo test --quiet $(RELEASE_PROFILE) --test your_venue_creation -- --nocapture >>$$log 2>&1; rc3=$$?; \
		if [ $$rc1 -ne 0 ] || [ $$rc2 -ne 0 ] || [ $$rc3 -ne 0 ]; then st=red; dt='implement src/your_venue/mod.rs + tests/your_venue_creation.rs'; \
		elif grep -q 'SKIP' $$log; then st=skipped; dt='set SOLANA_RPC_URL'; \
		else st=ok; dt='venue suite passed'; fi; \
		printf '  %-24s  %-8s  %s\n' 'Off-chain' "$$st" "$$dt"
	@log=target/log-venue-prog.txt; \
		cargo test --quiet $(PROGRAM) --release $(addprefix --test ,$(VENUE_ROUTE_TESTS)) -- --nocapture >$$log 2>&1; rc=$$?; \
		if [ $$rc -ne 0 ]; then st=red; dt='implement YourVenue + your program venue module'; \
		elif grep -q 'SKIP' $$log; then st=skipped; dt='needs fresh build-program + RPC + dumps'; \
		else st=ok; dt='route suite passed'; fi; \
		printf '  %-24s  %-8s  %s\n' 'On-chain program' "$$st" "$$dt"

# --- public targets -----------------------------------------------------------
# The route program builds as SBPF v3, as send.fun's programs do, with
# cargo-build-sbf 4.3.0 in place of `anchor build`. build-sbf downloads the
# platform tools. The rustc wrapper is off: build-sbf swaps compilers
# mid-build, and a wrapper cache can serve objects from the wrong one.
build-program:
	@cd program-template/programs/titan-v3-venue-template && \
		PATH="$$HOME/.cache/solana/$(PLATFORM_TOOLS)/platform-tools/rust/bin:$$PATH" \
		RUSTC_WRAPPER= CARGO_BUILD_RUSTC_WRAPPER= \
		cargo build-sbf --tools-version $(PLATFORM_TOOLS) --arch v3 --no-rustup-override \
			--sbf-out-dir "$(CURDIR)/program-template/target/deploy"

check-structure: _unit-phase

test-example: _example-sim-phase
	@echo
	@SCORECARD_SECTION=example $(SCORECARD)

test-venue: _venue-phase
	@echo
	@SCORECARD_SECTION=venue $(SCORECARD)

scorecard:
	@SCORECARD_SECTION=both $(SCORECARD)

# Dump each program binary into programs/ if it isn't already there. Requires the
# `solana` CLI on PATH.
dump-programs:
	@mkdir -p programs
	@for p in $(PROGRAMS); do \
		if [ -f programs/$$p.so ]; then \
			echo "have programs/$$p.so"; \
		else \
			echo "dumping $$p from $(DUMP_URL)"; \
			solana program dump -u $(DUMP_URL) $$p programs/$$p.so; \
		fi; \
	done
