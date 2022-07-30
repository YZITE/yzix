perllibpfx="$out/lib/perl5/5.34"
set -xe
unset src
sh Configure -des                          \
    -Dusedevel                             \
    -Uversiononly                          \
    -Dusethreads                           \
    -Dprefix="$out"                        \
    -Dvendorprefix="$out"                  \
    -Dprivlib="$perllibpfx"/core_perl      \
    -Darchlib="$perllibpfx"/core_perl      \
    -Dsitelib="$perllibpfx"/site_perl      \
    -Dsitearch="$perllibpfx"/site_perl     \
    -Dvendorlib="$perllibpfx"/vendor_perl  \
    -Dvendorarch="$perllibpfx"/vendor_perl

make -j4
make install
