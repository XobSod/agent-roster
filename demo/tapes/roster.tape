# The README's GIF: going through the sessions that need you while the shelves
# re-sort as agents work, and a menu. The agents are demo/env/fake-claude; the
# timing of their scenarios fits this tape.
Output demo/gifs/roster.gif
Source demo/tapes/settings.tape

Hide
Type "source demo/env/setup.sh && clear && roster"
Enter
Wait+Screen@90s /Needs input \(1\)/
Sleep 1s
Show

# A finished turn you haven't seen yet, and agents working.
Sleep 2.5s
Type "j"
Sleep 300ms
Type "j"
Sleep 1.5s
Type "k"
Sleep 300ms
Type "k"
Sleep 1.2s

# Read it, give it a follow-up, and come back.
Enter
Wait+Screen@10s /(^|\n)╭─/
Sleep 2.5s
Type "Also add tests for leap years"
Sleep 600ms
Enter
Sleep 1.5s
Ctrl+Q

# Meanwhile a question came in; the cursor waits on it.
Wait+Screen@10s /Needs input \(1\)/
Sleep 2.5s
Enter
Wait+Screen@10s /(^|\n)╭─/
Sleep 1.5s
Type "1"
Wait+Screen@20s /Logins are now limited/
Sleep 1.5s
Ctrl+Q

# The follow-up finishes and comes back up, unread.
Wait+Screen@10s /roster/
Wait+Screen@30s /Needs input \(1\)/
Sleep 3s

# Menus pop up their keys.
Type "s"
Sleep 1.8s
Type "n"
Sleep 2.5s
Escape
Sleep 1.5s
