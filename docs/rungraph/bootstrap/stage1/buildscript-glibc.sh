#!@bootstrapTools@/bin/bash -xe

updateSourceDateEpoch() {
    local path="$1"

    # Get the last modification time of all regular files, sort them,
    # and get the most recent. Maybe we should use
    # https://github.com/0-wiz-0/findnewest here.
    local -a res=($(find "$path" -type f -not -newer "$NIX_BUILD_TOP/.." -printf '%T@ %p\0' \
                    | sort -n --zero-terminated | tail -n1 --zero-terminated | head -c -1))
    local time="${res[0]//\.[0-9]*/}" # remove the fraction part
    local newestFile="${res[1]}"

    # Update $SOURCE_DATE_EPOCH if the most recent file we found is newer.
    if [ "${time:-0}" -gt "$SOURCE_DATE_EPOCH" ]; then
        echo "setting SOURCE_DATE_EPOCH to timestamp $time of file $newestFile"
        export SOURCE_DATE_EPOCH="$time"

        # Warn if the new timestamp is too close to the present. This
        # may indicate that we were being applied to a file generated
        # during the build, or that an unpacker didn't restore
        # timestamps properly.
        local now="$(date +%s)"
        if [ "$time" -gt $((now - 60)) ]; then
            echo "warning: file $newestFile may be generated; SOURCE_DATE_EPOCH may be non-deterministic"
        fi
    fi
}

first_arg() { echo "$1"; }

BOOT_LIB="@bootstrapTools@/lib"
if test -f $BOOT_LIB/ld.so.?; then
   # MIPS case
   LD_BINARY=$BOOT_LIB/ld.so.?
elif test -f $BOOT_LIB/ld64.so.?; then
   # ppc64(le)
   LD_BINARY=$BOOT_LIB/ld64.so.?
else
   # i686, x86_64 and armv5tel
   LD_BINARY=$BOOT_LIB/ld-*so.?
fi
LD_BINARY="$(eval echo $LD_BINARY)"

NIX_WRAPPER_gcc_ARGS="-Wl,--dynamic-linker=$LD_BINARY"
unset BOOT_LIB LD_BINARY

for dep in $buildInputs; do
  [ -d "$dep/lib" ] && NIX_WRAPPER_gcc_ARGS+=" -Wl,--rpath=$dep/lib" || true
  [ -d "$dep/include" ] && NIX_WRAPPER_gcc_ARGS+=" -I $dep/include" || true
  [ -d "$dep/bin" ] && PATH+=":$dep/bin" || true
done

[ -d "$gcc/lib" ] && NIX_WRAPPER_gcc_ARGS+=" -Wl,--rpath=$gcc/lib" || true
[ -d "$gcc/include" ] && NIX_WRAPPER_gcc_ARGS+=" -I $gcc/include" || true

for path in "$gcc"/lib/gcc/x86_64-*-linux-gnu/*/include \
  "$gcc"/lib/gcc/x86_64-*-linux-gnu/*/include-fixed; do
  [ -d "$path" ] && NIX_WRAPPER_gcc_ARGS+=" -idirafter $path" || true
done

NIX_WRAPPER_gcc_ARGS+=" -Wl,--rpath=$out/lib -Wl,--rpath=@bootstrapTools@/lib"

@bootstrapTools@/bin/mkdir -p /build/wrappers

# genWrapper name elem elemshv
genWrapper() {
  echo "#!@bootstrapTools@/bin/bash" > "$1"
  echo "exec $(type -p "$gcc/bin/$2") \$NIX_WRAPPER_${3}_ARGS \"\$@\"" >> "$1"
  @bootstrapTools@/bin/chmod +x "$1"
}
genWrapper gcc gcc gcc
genWrapper cc gcc gcc
genWrapper g++ g++ gxx
genWrapper cpp g++ gxx
genWrapper cxx g++ gxx

PATH+=":/build/wrappers:@bootstrapTools@/bin"

NIX_WRAPPER_gxx_ARGS="$NIX_WRAPPER_gcc_ARGS"

for path in "$gcc"/include/c++/* \
  "$gcc"/include/c++/*/x86_64-*-linux-gnu \
  "$gcc"/include/c++/*/backward; do
  [ -d "$path" ] && NIX_WRAPPER_gxx_ARGS="$NIX_WRAPPER_gxx_ARGS -idirafter $path" || true
done

export PATH NIX_WRAPPER_gxx_ARGS NIX_WRAPPER_gcc_ARGS
unset gcc

ln -sT @bootstrapTools@/bin /bin

if ! [ -d "$src" ]; then
  if [ -e "$(first_arg *)" ]; then
    mv -t/tmp *
    tar --no-same-owner -xf "$src" -C .
    export sourceRoot="$(ls | head -1)"
    mv -t. /tmp/*
  else
    tar --no-same-owner -xf "$src" -C .
    export sourceRoot="$(ls | head -1)"
  fi
  echo sourceRoot is "$sourceRoot"
  updateSourceDateEpoch "$sourceRoot"
  cd "$sourceRoot"
  if [ -f ../patches ]; then
    while read path; do
      echo "Applying patch $path ..."
      patch -p1 "$path"
    done < ../patches
  elif [ -d ../patches ]; then
    for path in ../patches/*; do
      echo "Applying patch $path ..."
      patch -p1 "$path"
    done
  fi
else
  cd "$src"
fi

env

### BEGIN main part

cd ..
mkdir build
cd build
if ! "../$sourceRoot/configure" --prefix="$out" \
    --build=$(../$sourceRoot/scripts/config.guess) \
    --enable-kernel=4.0 \
    --with-headers=$lnxheaders/include \
    --enable-languages=c,c++ \
    libc_cv_slibdir="$out/lib" \
    ; then
  echo "config.log:"
  cat config.log || true
  echo "-- config.log"
  exit 1
fi

make -j4
make install

# Fix hardcoded path to the executable loader in ldd script
sed '/RTLDLIST=/s@/usr@@g' -i "$out"/bin/ldd

### END main part

RES_LIB="$out/lib"
if test -f $RES_LIB/ld.so.?; then
   # MIPS case
   LD_BINARY=$RES_LIB/ld.so.?
elif test -f $RES_LIB/ld64.so.?; then
   # ppc64(le)
   LD_BINARY=$RES_LIB/ld64.so.?
else
   # i686, x86_64 and armv5tel
   LD_BINARY=$RES_LIB/ld-*so.?
fi
LD_BINARY="$(eval echo $LD_BINARY)"
echo new LD_BINARY="$LD_BINARY"
if ! [ -x $LD_BINARY ]; then
  echo "... is invalid"
  exit 1
fi

for path in $(find "$out/bin" "$out/lib" -type f -executable); do
  echo Mangling "$path"
  chmod +w "$path" || true
  # the bootstrap patchelf does not support all the options we want to use...
  patchelf --set-interpreter "$LD_BINARY" "$path" || true
  patchelf --shrink-rpath "$path" || true
  #patchelf --debug --add-rpath "$out/lib" "$path" || true
  #patchelf --debug --allowed-rpath-prefixes "$ZZZ_ALLOWED_RPATH" --shrink-rpath "$path" || true
  patchelf --print-needed "$path" || true
  old_rpath="$(patchelf --print-rpath "$path")" || continue
  new_rpath="$(echo "$old_rpath" | sed -e "s#@bootstrapTools@/#$out/#g")"
  if [ "x$(dirname "$path")" = "x$out/lib" ] && ( echo "$new_rpath" | grep -q "\$ORIGIN" ); then
    true
  else
    [ -n "$new_rpath" ] && new_rpath+=":"
    new_rpath+="$out/lib"
  fi
  patchelf --set-rpath "$new_rpath" "$path" || true
  patchelf --shrink-rpath "$path" || true
done
