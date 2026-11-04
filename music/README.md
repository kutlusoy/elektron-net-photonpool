# Background music

Optional music for the web pages (operator page, public pool page, miner page). Put audio files into this folder
(mp3, ogg, opus, m4a, wav, flac); the pages then show a player in the title bar (play, pause, stop, previous, next,
volume). The playlist loops. Without files here the pages use a built-in generated score instead.

Files are streamed by the browser, so they do not use memory on the server or in the page.

## Playlist and credits

By default the files are listed in alphabetical order with their file name as title. To set titles and the credit that a license
asks for, copy `playlist.example.json` to `playlist.json` and edit it. The credit is shown in a tooltip that opens when the
pointer rests on the title (or when the title is focused or tapped):

| Field | Meaning |
|---|---|
| `file` | file name in this folder (required, no sub-folders) |
| `title`, `artist` | shown in the player |
| `credit` | credit line the license asks for, for example `Music: 'Space Ranger' by Moire on #Uppbeat` |
| `url` | link to the artist page (https only) |
| `license` | license code of your download, if the site gives one |

## Licensing

Check the license of every track before you publish a pool page with it. Free tiers usually require a credit, and many
(for example Uppbeat) license a track for your own use, not for passing the file on. For that reason `.gitignore` keeps the audio
files and `playlist.json` out of git: copy them to the server yourself (`scp`, or the Docker bind mount `./music`).
Only commit a track that is released under a license that allows redistribution, for example CC0, and remove its line from `.gitignore` for that file.
