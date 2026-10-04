import Std.Time
open Std.Time

/-! V11 row group: `Std.Time` (chapter 05 section 3.5's `Std.Time` rows): a fixed instant shown in UTC and
in named zones read from the time-zone database (`TZDIR`, then the fixed zoneinfo paths; the TZif reader
and the zone rules are Lean code), the local zone from `TZ`, the lookup errors, and the current time
(`Timestamp.now`, compared only for order). Each date is rendered from Std.Time's own values (its
`PlainDateTime` fields, offset, abbreviation and weekday); Std.Time's format strings are not used here
(their formatter is a translator follow-up). -/

def fixed : Timestamp := Timestamp.ofSecondsSinceUnixEpoch 1700000000

def pad (n : Nat) : String := if n < 10 then s!"0{n}" else toString n

def render (dt : DateTime) : String :=
  let d := dt.toPlainDateTime
  let tz := dt.timezone
  s!"{d.year.toInt}-{pad d.month.toNat}-{pad d.day.toNat} {pad d.hour.toNat}:{pad d.minute.toNat}:{pad d.second.toNat} " ++
    s!"offset {tz.offset.second.val} {tz.abbreviation} ({tz.name}, dst {tz.isDST}) {repr dt.weekday}"

def zoned (label id : String) : IO Unit := do
  try
    let rules ← Database.defaultGetZoneRules id
    IO.println s!"{label}: {render (DateTime.ofTimestamp fixed rules)}"
    let summer := Timestamp.ofSecondsSinceUnixEpoch 1690000000
    IO.println s!"{label} in July: {render (DateTime.ofTimestamp summer rules)}"
  catch e =>
    IO.println s!"{label}: error: {e}"

def main : IO Unit := do
  IO.println s!"utc: {render (DateTime.ofTimestamp fixed TimeZone.ZoneRules.UTC)}"
  zoned "new york" "America/New_York"
  zoned "kolkata" "Asia/Kolkata"
  zoned "berlin" "Europe/Berlin"
  zoned "missing" "Mars/Olympus_Mons"
  try
    let localRules ← Database.defaultGetLocalZoneRules
    IO.println s!"local: {render (DateTime.ofTimestamp fixed localRules)}"
  catch e =>
    IO.println s!"local: error: {e}"
  -- the Windows time-zone externs answer Lean's non-Windows errors
  try
    let _ ← Database.Windows.getNextTransition "UTC" 0 true
    IO.println "windows transition: no error"
  catch e => IO.println s!"windows transition: {e}"
  try
    let _ ← Database.Windows.getLocalTimeZoneIdentifierAt 0
    IO.println "windows zone id: no error"
  catch e => IO.println s!"windows zone id: {e}"
  let now ← Timestamp.now
  IO.println s!"now after fixed: {decide (now > fixed)}"
  let later ← Timestamp.now
  IO.println s!"monotone enough: {decide (later ≥ now)}"
