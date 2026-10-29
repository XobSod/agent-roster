# Subagents: a card counts those at work, and unfolds to what each is doing.
Output demo/gifs/subagents.gif
Source demo/tapes/settings.tape

Hide
Type "source demo/env/setup.sh subagents && clear && roster"
Enter
Wait+Screen@90s /↳3/
Sleep 1s
Show

Sleep 2s
#: Down
Type "j"
Sleep 400ms
Type "j"
Sleep 400ms
Type "j"
Sleep 1.5s

# Unfolded: each one's model, its latest step, how long it's been at it.
#: Unfold its subagents
Type "l"
Sleep 8s

# All done: the card folds, the turn comes back up unread.
Wait+Screen@30s /Needs input \(1\)/
Sleep 3.5s
