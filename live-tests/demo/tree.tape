# Welcome GIF 1: hierarchy-preserving upload ("Send tree").
# Run via ./record.sh tree - it stages /tmp/demo, seeds TOFU, and writes
# the env file sourced below. Keystrokes are position-coupled to the
# fixtures/demo/project tree (dirs first, then case-insensitive
# alphabetical); if the fixtures change, re-walk the sequence.
Output demo/tree.gif

Set FontSize 15
Set Width 1200
Set Height 660
Set Padding 12
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Hide
Type ". /tmp/demo-env.sh && cd /tmp/demo && clear"
Enter
Sleep 1s
Show

Sleep 500ms
Type@80ms "ssh-files media-server:/data/incoming"
Sleep 800ms
Enter
Sleep 5s

# Focus the local pane and show off the project's structure.
Left
Sleep 1s
Tab
Sleep 1.2s
Down 3
Sleep 800ms
Tab
Sleep 1.2s

# Back to the top, select the whole project, send it with its tree.
Type "g"
Sleep 800ms
Space
Sleep 1s
Type "m"
Sleep 1.5s
Down 3
Sleep 1.2s
Enter
Sleep 8s

# Inspect the mirrored structure on the server.
Right
Sleep 1s
Tab
Sleep 1.5s
Down 3
Sleep 800ms
Tab
Sleep 1.5s
Type "g"
Sleep 2.5s

# Act 2: delete the uploaded copy from the server (x, confirm with y)...
Type "x"
Sleep 1.8s
Type "y"
Sleep 1.5s

# ...then send a single nested file, tree-wise. Left pane still has
# project and src expanded; rows: project, assets, data, src, lib.rs,
# main.rs, README.md - so main.rs is five below the top.
Left
Sleep 1s
Down 5
Sleep 800ms
Space
Sleep 1s
Type "m"
Sleep 1.5s
Down 1
Sleep 1.2s
Enter
Sleep 3s

# Tree mode recreated the full path: project/src/main.rs.
Right
Sleep 1s
Tab
Sleep 1.2s
Down 1
Sleep 800ms
Tab
Sleep 1.2s
Sleep 3.5s
