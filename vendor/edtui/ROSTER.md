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
- A line is wrapped in a view narrower than one of its characters (a
  tab, four wide, in two or three columns, or any in none): that
  character has a line of its own, where the wrapping went on without
  end, taking memory until there was none.
- Visual `d` and `c` take what's selected: the line break after it only
  if the selection goes past its line's last character (`v$`), where
  edtui's took it whenever the selection ended on that character, joining
  the lines. Lines changed leave an empty line, as vim's; lines deleted
  leave the cursor on a line still there, and one line at least. Visual
  `p` replaces the selection as vim's does (lines for lines, lines over
  text between its halves, text over lines a line of its own) and yanks
  what was selected; `P` (`ReplaceSelection`) keeps what was yanked.
- `x` (`RemoveChar`) and Delete yank what they take, as vim's do; `X` is
  `RemoveCharBefore`. Neither does anything, undo point or clipboard, where
  there's nothing to take (an empty line, a line's start for `X`).
- Text objects (`actions/object.rs`): `iw`/`aw`, `iW`/`aW`, `ip`/`ap`,
  quotes and brackets (`()`/`b`, `[]`, `{}`/`B`, `<>`), inner and around,
  with `d`, `c`, `y` and in visual mode, in place of edtui's (brackets nest
  and span lines; a word's letters are Unicode's). Characters leave the
  line break after them (`diw` of a line's only word leaves it empty);
  characters deleted over lines from the first's indent to the last's end
  are those lines (`daB` of a block on lines of its own), as vim's. `ciw`
  on an empty line goes into insert mode; with no such object nothing's
  done, and `.` (`EditorState::failed`) repeats what came before.
- `KeyEventHandler::pending()`: whether the next key goes on what came
  before it (a sequence begun, an operator waiting for its motion, an action
  waiting for its character), for keys of roster's own to know when they're
  edtui's.
- `r` (`Replace`) waits for its character as `f` does: any character, which
  `.` repeats.
- Counts (`3x`, `2dd`, `5j`, `d2w`, `3G`, `2p`, `3.`): the key handler
  takes digits before a command and within one (multiplied); an action
  takes one itself (`Execute::count`: the moves, `x`/`X`, `G`/`gg` to a
  line, an operator's motion, `i` and the like), or is done that many
  times in one undo step (`Repeat`), once if it enters insert mode. `dd`
  with a count yanks all its lines.
- Counts of their own, failing where vim's do rather than doing less: `f`
  and `t` (`FindForward`, `TillForward`) go to the `n`th (`2t,`), as `{`
  and `}` do paragraphs, `$` (`MoveToEndOfLine`) `n - 1` lines down, `r`
  (`Replace`) replaces `n` (`3rx`), the cursor on the last; a text object
  (`OnObject`) widens (`d2aw` two words, `2i(` the pair around the one
  around, `2i"` the quotes too), so the register holds all of it.
- `p`/`P` (`Put`, with a count): lines are put below the last line too, the
  cursor on the first one's first non-blank, as vim's; `2p` puts a block.
  `G` and `gg` go to a line's first non-blank. `P` of letters beyond
  ASCII lands on the last of them, not past it.
- `EditorState::last_change`/`set_last_change` (`LastChange`): what `.`
  repeats, for another editor to take, as vim's `.` is one for its buffers.
- Word motions as vim's `fwd_word`, `bck_word` and `end_word` go
  (`actions/word.rs`): `w` on to the next line's first word, not its
  start; `b` from a line's start to the last word before it; a word's
  letters Unicode's (`zażółć` one word, not six). `W`, `B` and `E` move by
  WORDs, `F` and `T` find backward (the `n`th with a count); `^` is the
  first non-blank, and `_` (`MoveDownToFirst`) is vim's, a count's lines
  down.
- Operators take any motion (`actions/operator.rs`): `d`, `c` and `y` are
  `Operate`, waiting in the key handler for a motion (its count multiplied
  in: `2d3w`), a text object (`i`/`a` and its key), or their own key again
  for lines (`dd`, `3cc`, `yy`), in place of edtui's `dw`, `cw`, `df`,
  `diw`, `yy` and the like. A motion says how an operator takes it
  (`Execute::motion`: exclusive, inclusive, lines) and moves as it does
  after one (`Execute::move_after_operator`: `l` past the line's end, `w`
  to it, `j` not past the last line, `f` failing). Vim's rules: an
  exclusive motion ending at a line's start ends at the line before's end,
  or takes whole lines (`d}`); characters deleted from a line's indent to
  another's end are those lines (`de` from a blank line); `cw` is `ce`; a
  change of nothing (`cl` on an empty line, `ch` at a line's start) goes
  into insert mode; a linewise yank leaves the cursor where it was unless
  it went up (`yy`, `yk`); an empty note is as an empty line, and deleting
  every line leaves one. `D`, `C`, `S` and `s` are `d$`, `c$`, `cc` and
  `cl`. `.` repeats each, a count replacing its motion's (`3.` after
  `dw`), and not one that failed.
- `J` is `JoinLines`, as vim's: a space between lines (none after one,
  before a `)`, or with an empty line), the joined line's indent gone, and
  a count of lines (`3J` joins three, `J` two). edtui's
  `JoinLineWithLineBelow`, joining with nothing between, stays. On the
  last line it fails, and `.` repeats what came before.
- Counted inserts, as vim's: `i`, `a`, `I`, `A`, `o` and `O` are
  `EnterInsert`, and with a count what's typed is typed that many times in
  all on leaving insert mode (`3ifoo`, `2o` a line each), in one undo step;
  `.` does it again with the count, or `3.` with its own. Enter,
  Backspace, Delete, Ctrl+W and Ctrl+U in insert mode are recorded for
  `.` and a count too, as they weren't, and typed again as they were. In
  an insert session (`EditorState::insert_start`) Backspace and Delete are
  in its one undo step, not each in one of its own. Ctrl+W and Ctrl+U in
  insert mode are vim's (`DeleteWordBefore`, `DeleteLineBefore`): the word
  before the cursor, or what was typed before it in this insert (all
  before it if nothing was), not yanked, stopping once where the insert
  began.

## Not done
What vim has and roster's copy still hasn't: `;` and `,`; `~`, `gu`,
`gU`; `gJ`; `>` and `<` (edtui has no indenting); `.` in visual mode;
marks; `H`, `M` and `L`; a search, `Ctrl+D` or a page as an operator's
motion (`d/x`); `%` with a count (vim's goes to a line by percent); a
word object's count going on past its line's end, as vim's does.

Where it isn't vim's yet: arrows, Home and End typed in insert mode
aren't kept for `.` or a count; Ctrl+W and Ctrl+U are in the insert's
one undo step, not one each; `.` and a count type again a Backspace,
Delete or Ctrl+W that did nothing the first time (at the buffer's start,
or the insert's), deleting then, where vim types again only what was
left; `j` and `k` go by characters, not by columns on the screen, and
lose the column they keep when a count is typed before them (`3j`), and
after `$` keep a column rather than going to each line's end; `u` leaves
the cursor where the change began rather than where vim does; `yG` and
`ygg` leave it on another column than vim's; CJK characters are a
word's, not a class each; `aw` on a line's only word takes its indent.
