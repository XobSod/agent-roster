# The README's first GIF: going through the sessions that need you while the shelves
# re-sort as agents work, the list staying beside the agent you're in. The agents
# are demo/env/fake-agent; the timing of their scenarios fits this tape.
Output demo/gifs/overview.gif
Source demo/tapes/settings.tape

Hide
Type "source demo/env/setup.sh overview && clear && roster"
Enter
Wait+Screen@90s /Needs input \(1\)/
Sleep 1s
Show

# A finished turn you haven't seen yet, and agents working.
Sleep 2.5s
#: Down
Type "j"
Sleep 400ms
Type "j"
Sleep 1.5s
#: Up
Type "k"
Sleep 400ms
Type "k"
Sleep 1.2s

# Read it, give it a follow-up, and come back: the list stays where it was.
#: Attach: the list stays beside it
Enter
Wait+Screen@10s /C-q back/
Sleep 2.5s
Type "Also add tests for leap years"
Sleep 600ms
Enter
Sleep 1.5s
#: Back to the list
Ctrl+Q

# Meanwhile a question came in; the cursor waits on it. (Coming back, the list is
# drawn as it was before attaching for a moment: waited on what's new.)
Wait+Screen@15s /Input/
Sleep 2.5s
#: Attach
Enter
Wait+Screen@10s /C-q back/
Sleep 1.5s
Type "1"
Wait+Screen@20s /offset parameter still works/
Sleep 1.5s
#: Back
Ctrl+Q

# The follow-up finishes and comes back up, unread.
Wait+Screen@10s /roster/
Wait+Screen@30s /1 needs input · 1 working/
Sleep 3s

# Menus pop up their keys.
#: Session menu
Type "s"
Sleep 1.8s
#: New session
Type "n"
Sleep 2.5s
#: Cancel
Escape
Sleep 1.5s
