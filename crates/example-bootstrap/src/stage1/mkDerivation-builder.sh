#!@bootstrapTools@/bin/bash -e

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
  NIX_WRAPPER_gcc_ARGS="$NIX_WRAPPER_gcc_ARGS -Wl,--rpath=$dep/lib -I $dep/include"
  PATH="$PATH:$dep/bin"
done

for path in @bootstrapTools@/include-glibc \
  @bootstrapTools@/lib/gcc/i686-unknown-linux-gnu/8.3.0/include \
  @bootstrapTools@/lib/gcc/i686-unknown-linux-gnu/8.3.0/include-fixed; do
  NIX_WRAPPER_gcc_ARGS="$NIX_WRAPPER_gcc_ARGS -idirafter $path"
done

NIX_WRAPPER_gcc_ARGS="$NIX_WRAPPER_gcc_ARGS -Wl,--rpath=@bootstrapTools@/lib -I @bootstrapTools@/include -Wl,--rpath=$out/lib"
PATH="$PATH:@wrappers@/bin:@bootstrapTools@/bin"

NIX_WRAPPER_gxx_ARGS="$NIX_WRAPPER_gcc_ARGS"

for path in @bootstrapTools@/include/c++/8.3.0 \
  @bootstrapTools@/include/c++/8.3.0/i686-unknown-linux-gnu \
  @bootstrapTools@/include/c++/8.3.0/backward; do
  NIX_WRAPPER_gxx_ARGS="$NIX_WRAPPER_gxx_ARGS -idirafter $path"
done

export PATH
export NIX_WRAPPER_gxx_ARGS
export NIX_WRAPPER_gcc_ARGS

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

"$@" || exit 1

for path in $(find "$out/bin" "$out/lib" -type f -executable); do
  echo Shrinking RPATH of "$path"
  chmod +w "$path" || true
  patchelf --shrink-rpath "$path" || true
done
