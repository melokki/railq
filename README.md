# RailQ v0.1

RailQ is a small terminal game about operating a passenger railway company
over a public Rail Network. The playable loop is: buy a diesel Train, create
or reuse a Passenger Service, make a Manual Dispatch, wait for the Journey to
arrive, and use the resulting Company Funds to grow or recover the Fleet.

## Launch

From the repository root, run:

```text
cargo run --release
```

The development build can be launched with `cargo run`. RailQ uses the
directory from which it is launched as its save directory. The default save is
`railq.ron`; the sidecar `railq.ron.lock` prevents two RailQ processes from
owning the same save slot.

On a first launch in a directory without `railq.ron`, enter a Player Company
name. RailQ shows the generated Region and Concession, then saves the new game
before opening the dashboard. Company names are trimmed, must contain a
non-whitespace character, and can be at most 60 characters.

## Load and save behavior

Loading is automatic at startup:

- If `railq.ron` exists, RailQ validates and loads that exact Region, Fleet,
  Passenger Services, demand, Journeys, Company Funds, and saved balance rules.
- Startup reconciles elapsed time using the current UTC clock before showing
  the dashboard. A due Journey settles then, exactly once.
- If the file is missing, RailQ starts onboarding. It never silently replaces
  an existing company with a new one.
- A corrupt, unsupported, or invalid save is preserved and shown as a load
  error. Repair or move it aside, then restart RailQ.

There is no manual Save key. A new company and every accepted purchase,
Manual Dispatch, resale, or elapsed-time reconciliation are saved before the
result is shown. Saves are written through a temporary file and atomically
replace the old save; a failed write does not publish a partial action. Press
`Q` to leave the terminal after the latest accepted state has already been
saved.

## Keyboard

From the dashboard:

| Key | Action |
|---|---|
| `M` | Map: inspect the Region, Rail Lines, and Train locations |
| `T` | Fleet: inspect Trains and start a resale for a READY Train |
| `C` | Company: inspect Company Funds, totals, receipts, and financial status |
| `B` | Buy Trains: inspect the diesel catalogue and start a purchase |
| `D` | Start Manual Dispatch from the Map |
| `Enter` | Select or confirm the current step |
| `Up` / `Down`, `J` / `K` | Move the current selection |
| `Esc` | Cancel the current flow; close Help when Help is visible |
| `?` / `H` | Open or close keyboard Help |
| `Q` or `Ctrl-C` | Exit RailQ |

To dispatch, press `M`, then `D`: select a READY Train, select a destination,
review the Passenger Service and Journey quote, and press `Enter` to confirm.
`Esc` cancels without creating the Passenger Service or Journey. The quote is
revalidated at confirmation.

To buy a Train, press `B`, then `Enter`: choose a catalogue Train, choose its
delivery Rail Station, and press `Enter` on the confirmation. To resell, press
`T`, then `Enter`: choose a READY Train and press `Enter` again. A travelling
Train cannot be sold.

After Bankruptcy, normal operations are blocked. Press `R`, then `Enter` to
confirm a safe restart, or `Esc` to cancel. The former Player Company save is
preserved in a uniquely named backup before the fresh game is saved.

## Journeys, real time, and offline play

Normal Journeys use 1:1 real time. The departure quote shows the duration and
the departure cost (Infrastructure Access Fee plus Fuel Cost). Those costs and
the boarded Waiting Passengers are applied at departure. Operating Revenue is
credited only when the Train arrives, and the Train then becomes READY at its
destination. Journeys do not automatically dispatch again.

While RailQ is open, the terminal checks elapsed time about four times per
second, so a due Journey can settle while no key is pressed. The Map and Fleet
views show progress and ETA; the Company view shows settled Journey receipts.

Quitting does not pause a Journey. On a later launch, RailQ compares the saved
Journey with the current clock: before its ETA the Train remains travelling; at
or after its ETA the Journey settles once, the Train waits at its destination,
and the receipt is saved. A long offline gap increases each connected
direction's demand using elapsed-time arithmetic, capped at 24 hours of demand.
Offline time never creates an automatic departure or an idle charge.

## Reserve risk and financial failure

Buying a Train can leave too little Company Funds for its first Journey. The
Buy Trains confirmation shows the funds after purchase and displays a **LOW
RESERVE WARNING** when the sample departure cost cannot be covered. Review the
dispatch quote's Company Funds after departure as well: an empty or low-demand
Journey can still lose money because access and fuel costs are paid at
departure.

Insolvency means the Player Company cannot currently fund an available Journey,
but the Company view may list a finite recovery option. A READY Train can be
resold for 70% of its original purchase price, rounded down; a travelling Train
cannot be sold. Bankruptcy is reached only when no finite sell/retain/rebuy and
dispatch option can restore operation. An active Journey defers that final
assessment because its arrival may still provide Operating Revenue.

## Manual QA checklist

- [ ] From a clean subdirectory under the repository root with no `railq.ron`,
      run `cargo run --release`, create a named Player Company, confirm the
      Region/Concession summary, and verify that the dashboard opens.
- [ ] Quit with `Q`, relaunch from the same directory, and verify the same
      Player Company and Region load automatically.
- [ ] Open `B`, inspect both diesel Trains, choose a delivery Rail Station,
      confirm a purchase, quit, relaunch, and verify the Train remains in the
      Fleet with the reduced Company Funds.
- [ ] On `M`, press `D`, select the READY Train and a connected destination,
      inspect the quote, then press `Esc`; verify no funds, demand, Service, or
      Journey changed.
- [ ] Repeat dispatch and confirm it. Verify departure costs are deducted,
      Waiting Passengers are consumed, the Train shows TRAVELLING with an ETA,
      and no receipt appears before arrival.
- [ ] Leave RailQ open until the ETA. Verify the Train becomes READY at the
      destination and the Company view shows exactly one new receipt.
- [ ] Dispatch the returned Train manually in the opposite direction. Verify
      it does not depart automatically after the first arrival.
- [ ] Dispatch a Journey, quit before its ETA, wait past the displayed ETA,
      relaunch, and verify one receipt, a READY Train at the destination, and no
      duplicate receipt after another relaunch.
- [ ] In `B`, review a purchase that triggers LOW RESERVE WARNING. Also attempt
      an operation without enough Company Funds and verify the rejection leaves
      the state unchanged.
- [ ] In `T`, confirm resale of a READY Train and verify the displayed 70%
      proceeds are added to Company Funds. While another Train is travelling,
      verify its resale is unavailable.
- [ ] In `C`, verify operating totals, latest receipts, and the distinction
      between Operating, Insolvent, and Bankruptcy status. When Insolvent,
      verify any listed recovery option reflects the actual Fleet and Journey
      costs.
- [ ] In a Bankrupt state, verify normal operations are blocked, `R` presents
      a confirmation, `Esc` preserves the old save, and confirmed restart
      creates a fresh game while leaving a bankruptcy backup beside the save.
- [ ] Resize below 64 columns by 16 rows and verify the resize hint; restore
      the terminal size, press `?`, and verify Help opens and closes with
      `Esc`.
- [ ] Verify `Ctrl-C` and `Q` both restore the normal terminal after exit.

## Not in v0.1

This build does not provide infrastructure construction or expansion,
automatic or Scheduled Operation, competitors, transfers, electric stock,
loans, staff, maintenance, pricing controls, or demand for unconnected
Settlements. The public Rail Network and its Rail Lines are fixed for this
playable loop.
