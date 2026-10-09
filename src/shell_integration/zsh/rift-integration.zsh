# Rift zsh integration: OSC 133 (A/B/C/D) semantic prompts + OSC 7 cwd.
#
# Loaded from .zshenv, i.e. BEFORE the user's .zshrc. Hooks are therefore
# installed lazily by a one-shot precmd function that runs just before the
# first prompt, once .zshrc (oh-my-zsh, powerlevel10k, starship, ...) has
# finished. Because we never touch PS1/PROMPT, prompt frameworks that rebuild
# the prompt asynchronously keep working: the end-of-prompt mark (B) is
# emitted from the zle-line-init hook, which fires after the prompt is drawn.

[[ -n "$_RIFT_INTEGRATION_LOADED" ]] && return 0
[[ -t 1 ]] || return 0
typeset -g _RIFT_INTEGRATION_LOADED=1
typeset -g _rift_cmd_ran=0

# Percent-encode $1 into $REPLY (RFC 3986 unreserved chars + '/').
_rift_urlencode() {
  emulate -L zsh
  local LC_ALL=C str=$1 out= c i
  if [[ $str != *[^A-Za-z0-9/._~-]* ]]; then
    REPLY=$str
    return
  fi
  for (( i = 1; i <= ${#str}; i++ )); do
    c=${str[i]}
    if [[ $c == [A-Za-z0-9/._~-] ]]; then
      out+=$c
    else
      out+=$(printf '%%%02X' "'$c")
    fi
  done
  REPLY=$out
}

_rift_osc7() {
  builtin local REPLY
  _rift_urlencode "$PWD"
  builtin printf '\e]7;file://%s%s\a' "${HOST:-localhost}" "$REPLY"
}

# precmd: command finished (D), then new prompt (A) + cwd.
_rift_precmd() {
  builtin local s=$?
  if (( _rift_cmd_ran )); then
    builtin printf '\e]133;D;%d\a' "$s"
    _rift_cmd_ran=0
  fi
  builtin printf '\e]133;A\a'
  _rift_osc7
  return $s
}

# Escape $1 into $REPLY for OSC 633;E: `\` -> `\\`, `;` and control chars
# -> `\xNN` (so the payload never contains a raw terminator or separator).
_rift_escape() {
  emulate -L zsh
  local s=${1[1,8192]} out= c h
  if [[ $s != *[[:cntrl:]]* && $s != *'\'* && $s != *';'* ]]; then
    REPLY=$s
    return
  fi
  for c in ${(s::)s}; do
    case $c in
      '\') out+='\\' ;;
      ';') out+='\x3b' ;;
      [[:cntrl:]]) printf -v h '\\x%02x' "'$c"; out+=$h ;;
      *) out+=$c ;;
    esac
  done
  REPLY=$out
}

# preexec: user hit Enter on a real command; output starts now (C). The exact
# command text ($1, as typed) is sent first via OSC 633;E so Rift never has to
# scrape it off a screen that p10k/starship right prompts, transient prompts
# and autosuggestions have drawn on.
_rift_preexec() {
  _rift_cmd_ran=1
  builtin local REPLY
  _rift_escape "$1"
  builtin printf '\e]633;E;%s\a\e]133;C\a' "$REPLY"
}

# zle-line-init: prompt fully drawn, cursor sits where input starts (B).
_rift_line_init() {
  builtin printf '\e]133;B\a'
}

# One-shot: runs before the first prompt, after all of .zshrc has loaded.
_rift_bootstrap() {
  builtin local s=$?
  add-zsh-hook -d precmd _rift_bootstrap
  unfunction _rift_bootstrap 2>/dev/null
  # Run first so $? is still the user's command status, and so the marks
  # bracket whatever other precmd hooks (p10k, starship) print.
  precmd_functions=(_rift_precmd ${precmd_functions:#_rift_precmd})
  add-zsh-hook preexec _rift_preexec
  add-zsh-hook chpwd _rift_osc7
  if (( $+functions[add-zle-hook-widget] )); then
    add-zle-hook-widget line-init _rift_line_init
  fi
  # This first prompt: precmd_functions was already snapshotted, so emit A here.
  builtin printf '\e]133;A\a'
  _rift_osc7
  return $s
}

autoload -Uz add-zsh-hook add-zle-hook-widget 2>/dev/null
add-zsh-hook precmd _rift_bootstrap
