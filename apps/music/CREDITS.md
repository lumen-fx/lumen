# Credits

Music by Kevin MacLeod, licensed under CC BY 4.0.

The default "Featured" playlist ships three royalty-free tracks by
Kevin MacLeod (incompetech.com), delivered via Git LFS as OGG Vorbis:

| Track | Artist | Source | License |
| --- | --- | --- | --- |
| Carefree | Kevin MacLeod (incompetech.com) | https://incompetech.com/music/royalty-free/mp3-royaltyfree/Carefree.mp3 | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| Wallpaper | Kevin MacLeod (incompetech.com) | https://incompetech.com/music/royalty-free/mp3-royaltyfree/Wallpaper.mp3 | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |
| Cipher | Kevin MacLeod (incompetech.com) | https://incompetech.com/music/royalty-free/mp3-royaltyfree/Cipher2.mp3 | [CC BY 4.0](https://creativecommons.org/licenses/by/4.0/) |

The source MP3s were transcoded to OGG Vorbis (`libvorbis -q:a 4`, 44.1 kHz)
because the Lumen audio runtime decodes OGG Vorbis and WAV only.

The `pure` and `moving` playlists use synthetic test tones generated in-repo
(see `README.md`); those are not third-party works.
