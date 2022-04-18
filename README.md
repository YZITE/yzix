# yzix

Warning/disclaimer: these programs and libraries are highly experimental and
might (and probably will) break in almost any scenario.

The goal of this project is to combine the power of a content-addressed nix-like
store with some-what incremental and parallel compilation like ccache and distcc.

It is orientied around a client-server model, and in contrast to `yzix-v0` it
doesn't perform build graph walking on the server, but on the client instead,
to make horizontally scaling easier.

## supported platforms

Most UNIX-platforms where rustc works are supported. (I don't yet know which aren't.)

Currently, it doesn't support Windows, because containers, symlinks and
read-only files work much different from unix there.
