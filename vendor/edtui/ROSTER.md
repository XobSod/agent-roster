# edtui, as roster has it

edtui 0.11.7 (https://github.com/preiter93/edtui, MIT, `LICENSE.md`), as
published on crates.io, without its examples, screenshots and CI files,
and without the features roster doesn't use (`arboard`,
`syntax-highlighting`, `system-editor`, `arbitrary`) and their
dependencies in its manifest, so they aren't in roster's lock: the code
for them is as edtui has it, never built.
Roster's note editor is built on it; the changes roster makes to it are
listed here, so they can be told apart from edtui's own and offered
upstream.

## Changes
