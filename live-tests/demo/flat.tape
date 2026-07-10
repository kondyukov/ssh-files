# Welcome GIF 2: the flat-transfer pitch. Three per-shoot folders on the
# server, each holding a video and its metadata; expand each, cherry-pick
# just the videos, and `d` drops them flat into the local downloads dir -
# no wrapper folders. Run via ./record.sh flat.
Output demo/flat.gif

Set FontSize 15
Set Width 1200
Set Height 660
Set Padding 12
Set Theme "Catppuccin Mocha"
Set TypingSpeed 60ms

Hide
Type ". /tmp/demo-env.sh && cd /tmp/demo/downloads && clear"
Enter
Sleep 1s
Show

Sleep 500ms
Type@80ms "ssh-files media-server:/data/demo/shoot"
Sleep 800ms
Enter
Sleep 5s

# Focus starts on the remote pane. Expand each shoot folder and pick
# out its video (rows per folder: metadata.txt, then vid<n>.mp4).
Tab
Sleep 1.5s
Down 2
Sleep 600ms
Space
Sleep 900ms

Down 1
Sleep 600ms
Tab
Sleep 1.5s
Down 2
Sleep 600ms
Space
Sleep 900ms

Down 1
Sleep 600ms
Tab
Sleep 1.5s
Down 2
Sleep 600ms
Space
Sleep 1.2s

# Send flat: the three videos land side by side in downloads/.
Type "d"
Sleep 9s
Sleep 3.5s
