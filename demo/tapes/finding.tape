# Finding your way: a fuzzy filter, one project alone, and the list by project.
Output demo/gifs/finding.gif
Source demo/tapes/settings.tape

Hide
Type "source demo/env/setup.sh finding && clear && roster"
Enter
Wait+Screen@90s /Idle \(6\)/
Sleep 1s
Show

Sleep 2s
# / filters as you type, fuzzily.
#: Filter, fuzzily
Type "/"
Sleep 500ms
Type "chlog"
Sleep 2.5s
#: Keep the filter
Enter
Sleep 2s
#: Clear it
Escape
Sleep 1.5s

# One project alone: p f on one of its sessions.
Type "p"
Sleep 600ms
#: Only this project
Type "f"
Sleep 2.5s
Type "p"
Sleep 400ms
#: All of them again
Type "f"
Sleep 1.5s

# By project instead of by attention: a setting, changed at once.
Type "v"
Sleep 500ms
#: Settings
Type "s"
Sleep 2s
#: Down to group_by
Type "jjjjjj"
Sleep 1.2s
#: By project
Type "l"
Sleep 2s
#: Back
Escape
Sleep 3.5s
