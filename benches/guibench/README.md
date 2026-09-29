# guibench apps

The Lumen versions of the apps that
[guibench](https://github.com/lumen-fx/guibench) runs in every framework it
compares. guibench builds `lumenc` from a Lumen checkout and reads these apps
from it, so a change that breaks one is fixed here in the same pull request.

`textview/src/main.lmn` is an empty shell; guibench inserts its text corpus into a
copy before measuring.
