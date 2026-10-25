# Licence: signing in from the app (spec for `src-tauri/src/license_client.rs`)

How licensing works from this release on:

1. You buy CubeCursor on cubecursor.shop and get a key.
2. You redeem the key to a CubeCursor account on the website (Account page). From then on the key
   belongs to that account for good; nobody else can redeem it.
3. In the app you **sign in** with that account. Keys are never typed into the app any more:
   `/api/license/lease` now always answers `{ ok: false, reason: "sign_in_required" }`.
4. The licence runs on **one computer at a time** (the "main"). Signing in on another computer
   moves it there; the old one is told `moved` at its next renewal and must stop.
5. Trials: one 14-day trial per computer (`/api/license/trial`, by `machine_identity()`), unchanged.

The settings window already calls the commands below; they must exist and be registered in
`lib.rs`'s `invoke_handler`. All network replies are JSON. `hw` = `machine_identity()`,
`dev` = the node's device id, `name` = the computer's name. Base URL `https://cubecursor.shop`.

## Saved state (`license.json`)

Add to the saved licence: `token: Option<String>` (the sign-in token), `email: Option<String>`,
`moved_to: Option<String>`. Drop `key` (ignore it when reading old files). Keep `pass`, `exp`,
`kind`, `ends`, `last_error`.

`LicenseView` gains `email: Option<String>` and `moved_to: Option<String>` (the UI reads both).

## Commands

| Command | Does | Returns |
|---|---|---|
| `signin_start` | `POST /api/app/link/start { hw, dev, name }` → `{ code, poll, url, interval, expires_in }`. Keep `poll` in memory (never shown), open `url` in the browser. | `{ code, interval, expires_in }` |
| `signin_poll` | `POST /api/app/link/poll { poll }`. `pending` → `{status:"pending"}`. `denied`/`expired` → that status. `done` with `ok:true` → save `pass`, `exp`, `token`, `email`, `kind:"paid"`, clear `moved_to`, `node.set_license_pass(Some(pass))`. `done` with `ok:false` → `error` from `reason`: `no_license` → "This account has no licence yet. Redeem your key at cubecursor.shop/dashboard.", `revoked`, `too_many_moves`. | `{ status, view?, error? }` |
| `signin_cancel` | Forget the in-memory `poll`. | `()` |
| `signin_open` | Open the last `url` in the browser again. | `()` |
| `signout` | `POST /api/app/signout { token, hw }` (ignore network errors), then clear `token`, `email`, `pass`, `exp`, `kind`, `moved_to`, `node.set_license_pass(None)`. | `LicenseView` |
| `license_use_here` | `POST /api/app/renew { token, hw, dev, name, takeover: true }` and apply like a renewal. | `LicenseView` |

Remove `license_activate` (keys) from the UI's reach; keep `license_trial`, `license_release`
can go (sign out replaces it).

## Renewal (replace the key branch in `start_renewal`)

At start and every hour, if `token` is set: `POST /api/app/renew { token, hw, dev, name }`.

- `ok: true` → save `pass`/`exp`, `set_license_pass(Some(pass))`, clear `moved_to`, `last_error`.
- `reason: "moved", on` → **stop now**: clear `pass`/`exp`, `set_license_pass(None)`,
  set `moved_to = on`. (This is what makes "not at the same time" hold while online.)
- `reason: "signed_out"` → clear `token`, `email`, `pass`, `exp`; `set_license_pass(None)`.
- `reason: "revoked"` → clear `pass`, `exp`; `set_license_pass(None)`; `last_error` = "This licence was refunded or revoked."
- Network error → keep the pass until it expires (passes now last 7 days offline).

If there is no `token`: renew the trial as today (`/api/license/trial`). A `trial_over` reply
clears the pass.

## Also

- Turn licence checking on in `lib.rs`: `license_key: Some(tandem_core::license::PUBLIC_KEY)`
  (today it is `None`, which means every copy works for free). Do this together with the above,
  and test on two PCs first: if renewal breaks, the computers disconnect.
- Refuse a pass whose `iat` is later than the clock (the clock was set back): treat as expired.
