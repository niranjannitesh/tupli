# Bundled themes

These are theme files, not source. Each is copied unmodified from where it was
published, and each keeps the `LICENSE` it shipped with — the copyright is the
theme author's, not ours:

| Family  | Author                | Copyright              | Licence                    |
| ------- | --------------------- | ---------------------- | -------------------------- |
| one     | Zed Industries        | (c) 2014 GitHub Inc.   | MIT (`one/LICENSE`)        |
| ayu     | Konstantin Pschera    | (c) 2016 Ike Ku        | MIT (`ayu/LICENSE`)        |
| gruvbox | Pavel Pertsev         | (c) 2018 Pavel Pertsev | MIT (`gruvbox/LICENSE`)    |
| fleet   | Lihuen Molina         | —                      | **none declared** — see below |

The first three came from the Zed editor's own `assets/themes`. `fleet` came
from the `skarline/zed-fleet-themes` extension, which ships no licence file and
whose repository has none either; it is bundled here because it was asked for,
and it is the one family in this directory that would need permission sorting
out before the app is distributed.

They are read by `crates/ui/src/zed_theme.rs`, which maps Zed's roles onto this
app's. Nothing here is edited: a bundled theme that needs fixing gets fixed by
dropping a file of the same name into `~/Library/Application Support/tupli/themes`,
which is where a user's own themes go and which wins over anything bundled.
Keeping these byte-identical to upstream is what makes updating them a copy.
