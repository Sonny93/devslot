#!/bin/sh
# A stand-in for `docker`, selected through DEVSLOT_DOCKER in integration tests.
# It answers `compose ... config --variables` and `compose ... config --format json`
# from fixture files keyed by the first `-f` file name:
#   $DOCKER_FAKE_FIXTURES/<file>.variables.json and <file>.config.json
# The placeholder __ROOT__ in the config fixture becomes the current directory.
# When $DB_VOLUME is set and $DOCKER_FAKE_FIXTURES/<file>.config.$DB_VOLUME.json
# exists, that config is answered instead: it lets a test make the answer depend
# on a variable the probe environment carries.
# Files named in $DOCKER_FAKE_FAIL (space separated) are rejected, with
# $DOCKER_FAKE_STDERR (when set) on stderr.

fixtures="${DOCKER_FAKE_FIXTURES:?DOCKER_FAKE_FIXTURES is not set}"
mode=""
first_file=""
previous=""
for argument in "$@"; do
  if [ "$previous" = "-f" ] && [ -z "$first_file" ]; then
    first_file="$argument"
  fi
  case "$argument" in
    --variables) mode="variables" ;;
    --no-env-resolution) mode="config" ;;
  esac
  previous="$argument"
done

for failing_file in ${DOCKER_FAKE_FAIL:-}; do
  if [ "$failing_file" = "$first_file" ]; then
    echo "${DOCKER_FAKE_STDERR:-fake docker rejected $first_file}" >&2
    exit 1
  fi
done

case "$mode" in
  variables) cat "$fixtures/$first_file.variables.json" ;;
  config)
    config_file="$fixtures/$first_file.config.json"
    variant="$fixtures/$first_file.config.${DB_VOLUME:-}.json"
    if [ -n "${DB_VOLUME:-}" ] && [ -f "$variant" ]; then
      config_file="$variant"
    fi
    sed "s|__ROOT__|$(pwd -P)|g" "$config_file"
    ;;
  *)
    echo "fake docker: unsupported call" >&2
    exit 64
    ;;
esac
