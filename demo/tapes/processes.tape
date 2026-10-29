# The server view: what each session's processes use, a runaway build marked as it
# passes the memory warning, and killed alone, its agent going on.
Output demo/gifs/processes.gif
Source demo/tapes/settings.tape

Hide
Type "source demo/env/setup.sh processes && clear && roster"
Enter
Wait+Screen@90s /Working \(1\)/
Sleep 1s
Show

Sleep 2s
# Its build grows past the memory warning: marked on its card, in its header, and
# in a line under the summary.
Wait+Screen@40s /⚠/
Sleep 4s

Type "v"
Sleep 600ms
#: The server view
Type "t"
Wait+Screen@5s /tmux sessions/
Sleep 1.5s

# The session unfolds to its process tree: the build is the one.
#: Down to the session
Type "jjjjjjjj"
Sleep 1.2s
#: Unfold its processes
Type "l"
Sleep 3s

# Killed alone, with what's under it.
#: Down to the build
Type "jj"
Sleep 1.2s
#: Kill it, and all under it
Type "X"
Sleep 2.5s
#: Yes
Type "y"
Sleep 3s

# Back in the list: the agent went on, and says why.
#: Back to the list
Type "q"
Sleep 1.5s
Wait+Screen@15s /Needs input \(1\)/
Sleep 3s
