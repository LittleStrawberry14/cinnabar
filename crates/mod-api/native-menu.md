# Trusted packet-mod menus

A WASI preview 1 packet reactor may export `menu(i32, i32) -> i32` alongside
`native-init(i32)`. The host sets capability bit `2` only when the mod has the
controls grant. Reactors without these optional exports retain their existing
packet ABI. Native-init and menu signatures are checked together before execution;
export discovery requests permissions but never grants them.

`menu` receives UTF-8 JSON bytes. Its return uses the same canonical
`result<list<u8>, string>` layout as other packet callbacks: a four-byte
discriminant, then the
pointer and length of the successful byte list or error string. Exporting
`cabi_post_menu(i32)` releases the returned allocation after copying it.

Input is bounded to 64 KiB:

```json
{"version":1,"open":true,"viewport":[640,400],"events":[{"id":"enabled","value":1}],"keys":[{"key":"KeyA","text":"a"}]}
```

The optional `viewport` is the available authored GUI size before reference-size fitting; it follows window resizing and platform safe-area insets.

Version one starts with only these original fields, including for guests that
reject unknown JSON fields. To accept additional facts, return optional output
`input_extensions:["pointer_state","shift_state"]`. The next input includes only
those groups: `pointer_state` adds `pointer_held` and `pointer_cancelled`, and
`shift_state` adds `shift_held`. Omission retains the accepted groups; an empty
array clears them. Requests must contain unique known groups. A new session starts
with no extensions. Each group's version-one field set is frozen; future facts
require a new opt-in group so existing strict decoders remain valid.

The host delivers native control IDs and values. Focused menu keys may carry
composed text; native number and choice editors receive their keyboard input
first. `capture_key` lets the guest handle Escape and the panel's toggle key while
open. The toggle key reopens the retained menu while closed. Other screens and
unfocused windows receive no guest menu keys.

Text controls are rejected before the panel is published. Version one delivers
numeric control events; committed text values need a separate input contract.

Output is bounded to 512 KiB. JSON `null` retains the current menu. A changed
output contains `open`, optional `panel` using `ui::mod_panel::Panel`, and optional `keys`:

```json
{"open":true,"panel":{"title":"Example","toggle_key":"ShiftRight","dark":true,"controls":[]},"keys":["KeyR"]}
```

Panel `motion` and authored surface transitions belong to the mod and remain
independent of Cinnabar's built-in Screen animations setting. Mods implement their
own motion preference by choosing `motion` and which transitions to declare.

Omitting `panel` retains its previous document. A retained panel's static catalog
is prepared on the packet worker; scalar updates reuse it. Closing or replacing
a session retires its pending input and prepared presentation.

An authored `surface.screen` identifies a logical route. Catalog revisions of the
same screen retain binding tweens; a different screen starts its declared entry.
Closing animates the displayed surface before a retained future route is installed.

`keys` subscribes to physical key press edges while focused gameplay owns input.
Those keys are reserved from ordinary gameplay, never collected from chat or
another menu. Each native packet session has its own menu identity, input queue
and prepared catalog; reconnecting or revoking grants retires its pending input
and retained presentation. Calls run on the existing serial packet worker.
Consecutive held-pointer edits of declared sliders retain their latest values
while the worker is busy. Buttons, keys, cancellation, visibility changes and
newly delivered panels preserve their ordering boundaries.

Custom panels support surface bindings, eased transitions, glass and paint
transforms through the shared JSON-UI engine. The `mod_panel_sans` font alias selects the shipped Inter outline face. `mod.control:N` invokes declared
control N. `mod.vector:X:Y` maps pointer position within its hit region to two
slider controls. `mod.drag:X:Y` preserves the initial grab offset and maps pointer
deltas across the available authored travel (reference dimensions minus the
handle's painted bounds) to the same bounded sliders. Capture freezes that travel
until release, so position and scale updates cannot change its gain.
These routes use the full authored root coordinate space; keep an interactive
placement canvas untransformed and size it to that reference. A widget's own
paint scale is included in its available travel.
Relative routes emit their current numeric values on press, without rounding or
moving them, so a guest can begin selection and an exact gesture rollback snapshot.
`mod.drag_axis:N:x` or `mod.drag_axis:N:y` changes one slider relatively across the
full authored reference dimension, suitable for a scale handle without a grab jump.
An optional positive reference extent, as in `mod.drag_axis:N:x:112`, replaces
that dimension when an authored handle needs a specific sensitivity.
The extent may instead name a declared positive numeric surface binding, as in
`mod.drag_axis:N:x:#resize_extent`; its value is captured on press, so geometry
updates during the gesture keep the same sensitivity and stable document.
Input `pointer_held` reflects focused menu capture, including release and focus
loss without another control edit.
`pointer_cancelled` marks an interrupted accepted gesture, such as lost focus,
cursor loss or another screen taking input. It is delivered once, including when
several samples coalesce; normal release leaves it false. `shift_held` is the
current focused physical Shift state. Ordered visibility phases retain earlier
edits while rejecting their stale panel results, so closing before a worker poll
cannot silently lose a final control edit.
The host owns drawing, input routing and animation; the guest owns feature state.

The Mods library detects the native callback permissions of raw reactors when
no explicit `cinnabar:grants` section exists. A native menu requests Controls.
Requests still require user consent and apply to a new server session. An explicit
grant section stays authoritative, including when it omits native Controls.

For a window-free layout capture, run:

```sh
cargo run -p ui-snapshot -- --panel panel.json --out panel.png \
  --width 1280 --height 720 --gui-scale 2 --time 1 --carriers /path/to/compiled
```

The CPU snapshot path uses the production panel painter and local validated
carriers. A headless client capture verifies the GPU glass/backdrop appearance.
Use `--resource-pack /path/to/pack` to load static textures referenced by the
panel. Texture names must be relative without dot segments; reads use the pack file limit.
