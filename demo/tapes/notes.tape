# Notes: a session's, with a line pinned for the preview, and its project's.
Output demo/gifs/notes.gif
Source demo/tapes/settings.tape

Hide
Type "source demo/env/setup.sh notes && clear && roster"
Enter
Wait+Screen@90s /Idle \(3\)/
Sleep 1s
Show

Sleep 1.5s
#: Down
Type "j"
Sleep 1.5s

# The session's note opens under the preview's header, with vim's keys.
Type "s"
Sleep 600ms
#: The session's note
Type "o"
Wait+Screen@5s /NORMAL/
Sleep 1.2s
#: Insert mode
Type "i"
Sleep 400ms
Type "- [x] wait for the total before paying"
Enter
Type "- [ ] the same race in the refund test"
Enter
Type "- [ ] 200 runs in CI before merging"
Sleep 700ms
#: Normal mode
Escape
Sleep 1.3s

# mm pins a line: the preview shows it and the one below.
Type "k"
Sleep 500ms
#: Pin this line
Type "mm"
Sleep 1.5s
#: Save and close
Type ":wq"
Sleep 400ms
Enter
Sleep 3s

# Its project's note, above it.
Type "p"
Sleep 600ms
#: The project's note
Type "o"
Wait+Screen@5s /NORMAL/
Sleep 1s
#: Insert mode
Type "i"
Sleep 400ms
Type "Release v2.3 on Friday; the API is frozen from Thursday"
Sleep 700ms
#: Normal mode
Escape
Sleep 1.3s
#: Save and close
Type ":wq"
Sleep 400ms
Enter
Sleep 3.5s
