# Usage: what's left of each account's limits, and what the agents spent.
Output demo/gifs/usage.gif
Source demo/tapes/settings.tape

Hide
Type "source demo/env/setup.sh usage && clear && roster"
Enter
Wait+Screen@90s /Idle \(3\)/
Sleep 1s
Show

Sleep 2s
Type "v"
Sleep 600ms
#: Usage
Type "u"
Wait+Screen@15s /5 hours/
Sleep 4s

# What they spent: the last 24 hours, 7 days, 30.
#: Spent: the last 24 hours
Type "l"
Sleep 3s
#: 7 days
Type "l"
Sleep 3s
#: 30 days
Type "l"
Sleep 4s
#: Back
Type "q"
Sleep 1.5s
