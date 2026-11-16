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
# The variables named in $DOCKER_FAKE_LOG_ENV (space separated) are logged too,
# as `NAME=value` words between that prefix and the arguments.
# The subcommands named in $DOCKER_FAKE_FAIL_COMMANDS (space separated, e.g.
# `up down`) exit 1 after logging.
# `ps` prints the file $DOCKER_FAKE_PS when set, else `[]`.
# `volume ls` prints the file $DOCKER_FAKE_VOLUMES when set, else nothing.
# `docker ps -aq` prints the file $DOCKER_FAKE_CONTAINERS when set, `network ls`
# prints the file $DOCKER_FAKE_NETWORKS when set; `docker rm` and `network rm`
# exit 0 after logging. Their names in $DOCKER_FAKE_FAIL_COMMANDS are
# `docker_ps`, `docker_rm`, `network_ls`, `network_rm`, `volume_ls`, `volume_rm`.
# `docker ps --filter label=com.docker.compose.project --format ...` (the
# machine-wide listing of `ds ls`) prints the file $DOCKER_FAKE_RUNNING_PROJECTS
# when set, one compose project name per line, else nothing. It fails with the
# same name as the other `docker ps` call, `docker_ps`.
# `up`, `down`, `rm` and `volume rm` exit 0 after logging.
# When $DOCKER_FAKE_SLEEP is set (seconds, decimals allowed), every `config`
# call sleeps that long before it answers.
# `compose -p <name> ... config --format json` (without --no-env-resolution)
# answers the config fixture with `devslot-probe` replaced by <name>, or the file
# $DOCKER_FAKE_MODEL when set (a model that only the real resolution, with .env
# loaded, would give).

mode=""
first_file=""
previous=""
for argument in "$@"; do
  if [ "$previous" = "-f" ] && [ -z "$first_file" ]; then
    first_file="${argument##*/}"
  fi
  case "$argument" in
    --variables) mode="variables" ;;
    --no-env-resolution)
      mode="config"
      is_probe="yes"
      ;;
  esac
  previous="$argument"
done

subcommand=""
project=""
previous=""
for argument in "$@"; do
  if [ "$previous" = "-p" ] && [ -z "$project" ]; then
    project="$argument"
  fi
  previous="$argument"
done

if [ "${1:-}" = "volume" ]; then
  subcommand="volume ${2:-}"
elif [ "${1:-}" = "network" ]; then
  subcommand="network ${2:-}"
elif [ "${1:-}" = "ps" ]; then
  subcommand="docker ps"
elif [ "${1:-}" = "rm" ]; then
  subcommand="docker rm"
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
  logged_environment=""
  for logged_name in ${DOCKER_FAKE_LOG_ENV:-}; do
    eval "logged_value=\"\${$logged_name:-}\""
    logged_environment="$logged_environment$logged_name=$logged_value "
  done
  echo "COMPOSE_PROJECT_NAME=${COMPOSE_PROJECT_NAME:-} $logged_environment$*" >>"$DOCKER_FAKE_LOG"
fi

if [ "$subcommand" = "config" ] && [ -n "${DOCKER_FAKE_SLEEP:-}" ]; then
  sleep "$DOCKER_FAKE_SLEEP"
fi

subcommand_word=$(echo "$subcommand" | tr ' ' _)
for failing_command in ${DOCKER_FAKE_FAIL_COMMANDS:-}; do
  if [ "$failing_command" = "$subcommand_word" ]; then
    exit 1
  fi
done

case "$subcommand" in
  up | down | rm | "volume rm" | "docker rm" | "network rm") exit 0 ;;
  "docker ps")
    case " $* " in
      *" --format "*)
        if [ -n "${DOCKER_FAKE_RUNNING_PROJECTS:-}" ]; then cat "$DOCKER_FAKE_RUNNING_PROJECTS"; fi
        exit 0
        ;;
    esac
    if [ -n "${DOCKER_FAKE_CONTAINERS:-}" ]; then cat "$DOCKER_FAKE_CONTAINERS"; fi
    exit 0
    ;;
  "network ls")
    if [ -n "${DOCKER_FAKE_NETWORKS:-}" ]; then cat "$DOCKER_FAKE_NETWORKS"; fi
    exit 0
    ;;
  ps)
    if [ -n "${DOCKER_FAKE_PS:-}" ]; then cat "$DOCKER_FAKE_PS"; else echo "[]"; fi
    exit 0
    ;;
  "volume ls")
    if [ -n "${DOCKER_FAKE_VOLUMES:-}" ]; then cat "$DOCKER_FAKE_VOLUMES"; fi
    exit 0
    ;;
esac

if [ "$subcommand" = "config" ] && [ -z "$mode" ]; then
  mode="config"
fi

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
    if [ -z "${is_probe:-}" ] && [ -n "${DOCKER_FAKE_MODEL:-}" ]; then
      cat "$DOCKER_FAKE_MODEL"
      exit 0
    fi
    config_file="$fixtures/$first_file.config.json"
    variant="$fixtures/$first_file.config.${DB_VOLUME:-}.json"
    if [ -n "${DB_VOLUME:-}" ] && [ -f "$variant" ]; then
      config_file="$variant"
    fi
    sed -e "s|__ROOT__|$(pwd -P)|g" -e "s|devslot-probe|${project:-devslot-probe}|g" "$config_file"
    ;;
  *)
    echo "fake docker: unsupported call" >&2
    exit 64
    ;;
esac
