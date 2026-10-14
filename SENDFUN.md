# send.fun on Titan

This fork of the integration template adds send.fun launchpad bonding curves
and send.fun DEX pools. It passes the template's shared suite and route suite
on five live markets across both platforms, as a registered partner and as
the default partner. An offline suite runs without RPC.

| Layer | Where |
| --- | --- |
| Creation parser | `src/sendfun/creation.rs`: `parse_pool_creations`; fixtures in `tests/your_venue_creation.rs` |
| Quote layer | `src/sendfun.rs` and `src/sendfun/`: `SendfunVenue`; `PoolProtocol::{SendfunLaunchpad, SendfunAmm}` |
| Route builder | `src/swap_route/mod.rs`: `Venue::Sendfun { dex, buy }`, `protocol_to_venue` |
| Program layer | `program-template/.../venues/sendfun.rs`, `Venue::Sendfun` in `state.rs`, the `perform_cpi_swap` arm |

`src/your_venue/mod.rs` re-exports the venue under the template's module name.
The venue uses [`sendfun-sdk`](https://github.com/send-fun/sendfun-sdk-rust).
The SDK's types are Solana 3. The venue converts them at its boundary. The
template's library code stays on Solana 2.2.

## Programs and markets

| Program | Address | Market account | Protocol |
| --- | --- | --- | --- |
| Launchpad | `5R1uFyEE4oxqkm7hJDqHq6gXaLVF9dxeF3LF4yz1sfLP` | `BondingCurve` at `["bonding_curve", base, quote]` | `send.fun Launchpad` |
| DEX | `84qj5FPZZdXkQy8mfowyg6RBZ3XKuTds6XS4ZYT1sfDX` | `Pool` at `["pool", base, quote]` | `send.fun AMM` |

`SendfunVenue::from_account` takes either account. It refuses an account with
another owner or another discriminator. Token 0 is the base mint. Token 1 is
the quote mint. A buy spends quote for base. A sell spends base for quote. The
base mint is a Token-2022 mint. The quote mint is WSOL or another registered
SPL Token or Token-2022 mint.

A curve trades while `Funding`. A buy that takes the last of its supply
completes the curve. The completed curve then migrates into a new DEX pool. A
pool trades while `Active`.

The suites run against these markets:

| Test file | Market | Quote mint |
| --- | --- | --- |
| `sendfun_curve_wsol` | curve `Eq52yham9BNMbz2pCcqpSFUNAfJZKJFPNEhqHaEdyy7L` | WSOL |
| `sendfun_curve_fee` | curve `ECxRrNU6Ggd73c3AD7UPd6ijfJYdULWFT2jDJZu5gzcd` | `PreLWGkkeqG1s4HEfFZSy9moCrJ7btsHuUtfcCeoRua`, Token-2022 with an uncapped transfer fee |
| `sendfun_dex_pool` | pool `ChJu4A7Gow1DDsWdgHvcpJtxDQQZvA5tavWKn8zZ9kY5` | WSOL |
| `sendfun_hostile` | curve `DgH8nF1MiQrTiCeNF6fYfAsNhEhqawN4Q9JNuAt2UoM6` and pool `nBB3wTa7qDsv2Pkrwg9zoNfWvcjtQ9J2MmdTPhnVW1Y`, both on hostile.xyz's platform | `AMC1qwR9KhiyrQBRPrxnfo4JfMeMZqEBvt5tgTytNNoc` |

Each file has a `_route` twin in the program template's `tests/`. They route
as a registered partner. `sendfun_default_partner` and
`sendfun_default_partner_route` run all five markets again as the default
partner. `make test-venue` runs all ten files. These files replace the
template's `your_venue` entry points. `make dump-programs` also dumps the two
send.fun programs.

## Partner

Every trade routes as one send.fun partner. A partner is a key that send.fun
registers on each platform, with its own fee schedule. The partner signs
every trade. The default partner is the exception: the System Program key,
`11111111111111111111111111111111`. It cannot sign, and it pays each
platform's unpartnered schedule. The venue assumes no partner, and routes as
the default partner only when it is named:

- Call `sendfun::set_partner(key)` once, at startup. `from_account` then
  routes as that key. Until then `from_account` fails. A second call with
  another key fails.
- Or build each venue with `SendfunVenue::from_account_with_partner`.

The System Program key names the default partner.

A platform is a launch surface. Each market names its platform. Two exist:
send.fun (`2PJedAsa7pCnScks2XM3U4o2nPaTvvBmi2553VcCnGWB`) and hostile.xyz
(`6fBeMZFvaq4sNXNGNpKW1wNZgN42v8M3NxA4bCrHNjam`). send.fun registers Titan's
key on every platform before Titan lists a market.

The partner's fees come from its `PartnerConfig`, the nexus
(`7rrCurqdWFesbzwtYPo95fM8waTtVXgXwCom45x1sfNX`) PDA
`["partner", platform_config, partner]` of the market's platform. A market
whose platform has no config for the partner is unroutable. The config of
another key is never used in its place. A config that names another partner
or another platform is no config: the programs refuse it. The nexus authority can change the
rates. Each update reads them.

The trade instruction names a registered partner as a read-only signer. The
route leg keeps it a signer, and the template's router passes a transaction
signer on in its CPI. The transaction must carry the partner's signature, or
the trade fails. The programs check only that the partner signed, so the key must be
one that only Titan can sign for.

The config sets the protocol rate, a pool's LP rate and the decay premium. It
does not set the creator fee rate. Each market holds its own, in
`BondingCurve.creator_fee_bps` or `Pool.creator_fee_bps`, set at its creation.
Each update reads it from the market account. The config's
`max_creator_fee_bps` limits a market's rate at its creation only. No trade
pays it, and a market's rate can be above it.

The simulations route each market twice, on both platforms: as send.fun's own
partner key,
`1TZSDDRKxjgEFatTYAbSgM8kxZbSAjTRPZxHYd7send`, and as the default partner. On
2026-10-13 send.fun's key had a config on both platforms, with a protocol rate
of 0 bps on both programs and no decay premium. The account snapshots in
`tests/fixtures/accounts/` hold the default partner's schedule. The offline
suite routes as the default partner, and as a test partner that it registers
with the same schedule:

| | Launchpad | DEX |
| --- | --- | --- |
| Protocol rate | 200 bps | 5 bps |
| LP rate | none | 0 bps |
| Decay premium | from 5,000 bps over the first 20 seconds of a curve | none |
| `max_creator_fee_bps` | 0 bps | 10 bps |
| A snapshot market's creator fee rate | 0 bps on each curve | 10 bps on the pool |
| A trade pays | 200 bps, plus the decay premium | 15 bps |

## The swap instruction

`generate_swap_instruction(request, user)` builds `buy_exact_in` or
`sell_exact_in` of `request.amount` with `min_amount_out` 0. Both programs
take the same 17 accounts:

```text
user, payer, bonding_curve | pool, base_mint, quote_mint, base_vault,
quote_vault, user_base_account, user_quote_account, partner, partner_config,
base_token_program, quote_token_program, associated_token_program,
system_program, event_authority, program
```

`user` and `payer` are both `user`. The instruction marks both read-only
signers. `partner` is the venue's partner: a read-only signer, unless it is the
default partner. The IDL marks `payer` writable, because `payer` pays to create a user
token account that has no data. The programs require a writable `payer` only
for that trade. Titan's router creates TitanPDA's accounts first, so the leg
takes no write lock on TitanPDA. A trade whose user token account is missing
fails with `ConstraintMut` (2000). The user token accounts are `user`'s ATAs of
each mint, under the mint's token program. Transfer-hook accounts follow the 17
accounts.

The program adapter, `venues/sendfun.rs`, builds the same instruction from
`Venue::Sendfun { dex, buy }`:

- The program is the DEX if `dex`, else the launchpad.
- The data is the 8-byte discriminator, then `amount_in` and `0` as
  little-endian `u64`s. `buy_exact_in` is
  `[250, 234, 13, 123, 213, 156, 19, 236]`. `sell_exact_in` is
  `[149, 39, 222, 155, 211, 124, 152, 26]`.
- The leg's account metas pass through unchanged.

## Quotes

`quote()` is exact-in only. `ExactOut` returns `ExactOutNotSupported`.

An `amount` of 0 returns no output at the spot price. The spot price is the
price at 0, with each transfer fee's rate at the start of the range.

`quote()` refuses an `amount` that delivers nothing. This occurs only below the
lower bound. The program accepts such an amount and pays nothing.

Otherwise `quote()` runs the SDK's quote of the market's program with the
market's state, the partner's fees and the clock of the last update.
It does not allocate. Each error carries a static string or a key.

- **Platform fee.** The partner's protocol rate, the partner's LP rate on a
  pool, and the market's creator fee rate, plus a decay premium. The premium
  falls from the market's creation to 0 at the end of the partner's decay
  window. The partner's `max_creator_fee_bps` is not part of the fee. A
  market's `creator_fee_mode` sets who claims the creator fee, not the rate.
- **Transfer fees.** The quote includes the Token-2022 transfer fees on the
  program's transfers into and out of the market's vaults, each at the landing
  epoch. `expected_output` is the amount that reaches the user's account. The
  quote excludes the router's transfers into and out of TitanPDA. `TokenInfo`
  carries each mint's fee at the current epoch for those transfers.
- **Supply cap.** A curve sells at most the supply left. A buy past it fills
  the rest of the supply. The quote sets `not_enough_liquidity`, and `amount`
  is the input that the fill spends. The program spends exactly that amount.
  The unspent input stays in TitanPDA's account. The filled curve completes
  and migrates. A buy that lands first takes supply, so a later buy can fill
  less than its quote. With `min_amount_out` 0, the program accepts it.
- **Curve sells.** A sell pays out at most the quote that the curve holds. A
  larger sell fails, in the quote and in the program. A curve with no buys has
  no sell range.
- **Reserves.** A curve prices off its virtual reserves. A pool prices off its
  base vault's balance and its `quote_reserves`. Each update reads the base
  vault.

`price` is the derivative of the SDK's output. `t` is a mint's transfer-fee
rate, `b` the platform fee rate, and `Rq`, `Rb` the pricing reserves:

```text
buy:  n = (x - fee_q(x))(1 - b)
      price = (1 - t_q)(1 - b) Rq Rb / (Rq + n)^2 (1 - t_b)
sell: i = x - fee_b(x)
      price = (1 - t_b)(1 - b) Rq Rb / (Rb + i)^2 (1 - t_q)
```

A transfer fee's `t` is its rate below its cap point, the transfer that first
pays `maximum_fee`. From the cap point on, `t` is 0 and the fee is flat.

## Bounds

`bounds()` returns the range certified at the last update. It does not search
or allocate.

**Upper bound.** The largest input that:

- quotes,
- is not supply-capped,
- keeps the input vault's balance within `u64` (a pool's otherwise runs to
  `u64::MAX`),
- stays on one side of each transfer fee's cap point.

The output is not concave across a cap point, so a range that spans one fails
the price checks. The range is the widest stretch between cap points that
passes the checks. Past a low cap point, the range runs to the supply left or
to the quote that the curve holds.

**Lower bound.** At least 1,000,000 atoms. At each update, the venue runs
Titan's checks against its own quotes:

- the mean-value check on a 64-point geometric grid over the range,
- the price-monotone and output-monotone checks on a 200-point geometric grid.

It doubles the lower bound until both pass. If the lower bound reaches the
upper bound, the direction has no range, and `bounds()` returns
`NoQuotableValue`.

`directions_num()` declares a direction only if it has a range. A curve with
no buys declares only the buy. `tradable_mints()` returns the base mint, then
the quote mint, from the market account, before the first update.

## Updates

`get_required_pubkeys_for_update` returns:

- the market,
- both mints,
- both vaults,
- the partner's `PartnerConfig`,
- each armed transfer hook's `ExtraAccountMetaList`,
- the Clock sysvar while the clock can change a fee: on the first update,
  until the platform fee's decay window closes, and while a mint's newer
  transfer fee takes effect at a later epoch.

An update can find a fee that the last clock does not cover, such as a
relaunched curve or a new transfer fee. That update then reads the clock. A
requested clock that is missing fails the update.

`update_state` is all or nothing. A cache error on an account the update
declared fails it, and the venue keeps its last state. An account that does
not decode also fails it, and halts the venue: the error repeats until the
account changes, so the last state would only go stale. These conditions make
the venue unroutable (`initialized()` false). Each quote, bound and swap
instruction then fails until a later update clears the condition:

- An account does not decode, such as a market or partner config after a
  change to its layout.
- The cache refuses an account that the update needs and did not declare: a
  newly armed hook's account list, the partner config of a curve relaunched
  on another platform, or the clock. The next update declares it.
- The curve is not `Funding`, or the pool is not `Active`.
- The market account is closed.
- The partner has no config on the market's platform.
- A mint is paused.
- A mint's new token accounts start frozen (`DefaultAccountState`).
- A transfer hook's account list is missing or does not resolve before the
  swap.
- The platform fee is out of range, or a mint's `TokenInfo` transfer fee
  differs from the fee the quote charges. Neither occurs on a valid account.

## Transfer hooks

Both programs forward a Token-2022 transfer hook's extra accounts after the 17
trade accounts. The venue resolves them offline from the hook's
`ExtraAccountMetaList`, which each update reads. These lists are unsupported,
and make the market unroutable:

- a list that reads the transfer amount,
- a list that needs a signer,
- a list that reads data only the swap can see.

`program_dependencies()` returns the market's program, SPL Token, Token-2022,
the ATA program, the System Program and each armed hook's program.

No live send.fun mint has a hook program. The launchpad's base mints have no
hook extension. The offline suite checks the hook accounts of a quote mint
with `spl-transfer-hook-example`'s account list. Titan's router cannot route a
hooked mint. See [Limits of the template](#limits-of-the-template).

A hook account can repeat the owner of either token account of its transfer:
the user or the market. The venue appends it read-only, and the hook receives
it read-only. A hook account that repeats a vault or a user token account, or
needs the market or the user writable, is unsupported.

## Limits of the template

These limits come from the template's router and interface.

- **A router-created token account of a fee-bearing mint.** When TitanPDA has
  no token account for a route's mint, the router creates one and closes it at
  the end. Token-2022 refuses to close an account that holds withheld transfer
  fees (`AccountHasWithheldTransferFees`). A route through a mint with a
  transfer fee fails in both directions. When TitanPDA's accounts exist, the
  same routes land at the quote.
- **The unspent input of a partial fill.** A buy that lands after another buy
  took most of a curve's last supply fills less than its quote (see
  [Quotes](#quotes)). The program spends only what the fill needs. The router
  leaves the rest in TitanPDA's input account. It counts each TitanPDA
  account's balance at the start of a route as reserved, so no later route
  spends the rest, and nothing refunds it. `swap_route_v3` takes no minimum
  output, so the route lands.
- **The router's transfers of a hooked mint.** The router calls
  `transfer_checked` between the user and TitanPDA without the hook's extra
  accounts. Token-2022 fails the transfer (`MissingAccount`). The send.fun leg
  alone carries the extra accounts, and lands in both directions.
- **`program_dependencies()` can change.** It adds a hook program when a mint
  arms a hook, which any update can find. `TradingVenue` cannot report this.
  Jupiter's interface reports it with `has_dynamic_accounts`.

## Changes to the template's harness

The template's library code is unchanged. Its harness changes in six places.
No change alters a tolerance. The second change alters what the route suite
expects.

1. **Token accounts carry their mint's account extensions.** Both commons
   built each token account as a bare 165-byte account. Token-2022 fails a
   transfer of a fee-bearing mint into an account without `TransferFeeAmount`
   (`InvalidState`). It fails a transfer of a hooked mint unless both accounts
   carry `TransferHookAccount`. `tests/common/token_account.rs` sizes and
   initializes each account with the extensions its mint requires
   (`ExtensionType::get_required_init_account_extensions`).
2. **The route suite expects the router's transfer fees.** The router calls
   `transfer_checked` for the input from the user to TitanPDA, and for the
   output back. With a Token-2022 fee, each transfer pays it. The suite now
   expects `quote(amount - fee_in(amount)).expected_output - fee_out(that)`,
   with each mint's fee at the epoch the suite syncs the clock to.
3. **The simulations run on LiteSVM 0.14.** send.fun's programs read their
   instruction data from register r2 (SIMD-0321). LiteSVM 0.7.1 and earlier
   do not set it, and the programs fault at `0xfffffffffffffff8`. LiteSVM 0.14
   takes Solana 3 types. `tests/common/svm.rs` wraps it behind the 0.6 API
   that the suites call. It serializes each Solana 2.2 transaction with
   bincode and deserializes it as Solana 3. It converts accounts, the clock,
   the blockhash and the compute budget field by field. LiteSVM is a
   dev-dependency.
4. **Test entry points per market.** `tests/sendfun_*.rs` and
   `tests/sendfun_*_route.rs` replace `your_venue.rs` and `your_venue_route.rs`,
   one per send.fun-platform market, and `sendfun_hostile.rs` for the two
   hostile.xyz markets. Each sets the partner to send.fun's own key.
   `sendfun_default_partner.rs` and `sendfun_default_partner_route.rs` run
   all five markets as the default partner. The Makefile's venue phase runs
   them all. Its structural phase also runs the offline suite,
   `tests/sendfun_offline.rs`.
5. **The route program builds as SBPF v3.** send.fun's programs are SBPF v3.
   `make build-program` builds the route program the same way, with
   `cargo build-sbf --tools-version v1.57 --arch v3` in place of
   `anchor build`. `Anchor.toml` no longer pins `solana_version`.
6. **The simulations leave a partner's signature empty.** Both commons signed
   each transaction with `Transaction::new_signed_with_payer` and the user's
   keypair. A send.fun trade also names the partner as a signer, and the
   suites hold no key for it. The simulations now build the transaction with
   `new_with_payer` and `partial_sign`. LiteSVM runs with signature
   verification off, as the template sets it.

The program template's route suite includes the root suite's
`tests/common/svm.rs` and `tests/common/token_account.rs` by path. LiteSVM 0.14
needs wincode 0.5. Both lockfiles pin `solana-clock`,
`solana-last-restart-slot` and `solana-slot-history` below 3.2 to keep it.

## Running the tests

```bash
make check-structure                  # no RPC: unit tests, scorecard, offline suite, enum parity

make build-program                    # cargo-build-sbf 4.3.0, platform-tools v1.57
export SOLANA_RPC_URL=<mainnet-url>
make dump-programs
make test-example
make test-venue
```

`make build-program` needs `cargo-build-sbf` 4.3.0 on `PATH`
(`cargo install cargo-build-sbf --version 4.3.0 --locked`). It downloads
platform-tools v1.57 and builds the route program as SBPF v3.
`make dump-programs` needs the Solana CLI.

`tests/sendfun_offline.rs` runs against mainnet snapshots of four markets'
accounts in `tests/fixtures/accounts/`. It runs no program. The quote math is
`sendfun-sdk`'s. It covers:

- Titan's construction, monotone, price-monotone and mean-value checks, copied
  from the shared suite, with `bounds()` and the edge quotes under
  `assert_no_alloc`;
- the price against finite differences of the quote;
- zero-input quotes, a trade that delivers nothing, a buy past a curve's
  supply, a transfer fee that changes at an epoch, a capped transfer fee, a
  relaunched curve and the decay window, a market whose creator fee rate
  differs from the partner's maximum, a quote mint with a transfer hook, and a
  hook that names either transfer owner;
- each unroutable state, a requested clock that is missing, a cache that
  serves only the accounts an update declares, a cache error on a declared
  account, and an account that does not decode;
- the partner: `from_account` with the partner that is set, the partner's own
  protocol rate, the partner as a signer of the trade and of the route leg,
  the default partner when it is named, a partner with no config while the
  default partner has one, and a config that names another partner or
  platform;
- the swap instruction, the route leg and the creation parser.
