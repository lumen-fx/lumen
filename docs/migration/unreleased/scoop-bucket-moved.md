# The Scoop bucket moved to the Lumen repository

This affects Windows users who installed Lumen with Scoop from the
`lumen-fx/scoop-lumen` bucket. The bucket is now the Lumen repository itself,
and new releases reach Scoop only there. The old bucket stays at the last
release it carried, so `scoop update lumen` stops finding anything newer.

Replace the bucket once, then update as before:

```
scoop bucket rm lumen
scoop bucket add lumen https://github.com/lumen-fx/lumen
scoop update lumen
```

Lumen itself stays installed while the bucket is replaced, and the app name is
still `lumen`, so nothing else changes.
