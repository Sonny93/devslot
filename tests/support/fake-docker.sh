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
#
# It also answers the commands that act on an instance. The subcommand is the
# first of `up`, `down`, `rm`, `ps`, `config` after the compose global flags, or
# `volume ls` / `volume rm`.
# When $DOCKER_FAKE_LOG is set, every call but `config` appends one line to that
# file: the arguments joined by single spaces, prefixed by
# `COMPOSE_PROJECT_NAME=<value of that variable, empty when unset> ` so a test
# can check that inherited COMPOSE_* variables were removed.
# The subcommands named in $DOCKER_FAKE_FAIL_COMMANDS (space separated, e.g.
# `up down`) exit 1 after logging.
# `ps` prints the file $DOCKER_FAKE_PS when set, else `[]`.
# `volume ls` prints the file $DOCKER_FAKE_VOLUMES when set, else nothing.
# `up`, `down`, `rm` and `volume rm` exit 0 after logging.

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

subcommand=""
if [ "${1:-}" = "volume" ]; then
  subcommand="volume ${2:-}"
else
  previous=""
  for argument in "$@"; do
    case "$previous" in
      -p | -f) ;;
      *)
        case "$argument" in
          up | down | rm | ps | config)
            subcommand="$argument"
            break
            ;;
        esac
        ;;
    esac
    previous="$argument"
  done
fi

if [ "$subcommand" != "config" ] && [ -n "${DOCKER_FAKE_LOG:-}" ]; then
  echo "COMPOSE_PROJECT_NAME=${COMPOSE_PROJECT_NAME:-} $*" >>"$DOCKER_FAKE_LOG"
fi

for failing_command in ${DOCKER_FAKE_FAIL_COMMANDS:-}; do
  if [ "$failing_command" = "$subcommand" ]; then
    exit 1
  fi
done

case "$subcommand" in
  up | down | rm | "volume rm") exit 0 ;;
  ps)
    if [ -n "${DOCKER_FAKE_PS:-}" ]; then cat "$DOCKER_FAKE_PS"; else echo "[]"; fi
    exit 0
    ;;
  "volume ls")
    if [ -n "${DOCKER_FAKE_VOLUMES:-}" ]; then cat "$DOCKER_FAKE_VOLUMES"; fi
    exit 0
    ;;
esac

fixtures="${DOCKER_FAKE_FIXTURES:?DOCKER_FAKE_FIXTURES is not set}"

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
