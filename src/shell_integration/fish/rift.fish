# Rift fish integration, loaded with `fish --init-command 'source <this>'`.
# Never edits user config. fish >= 4.0 emits OSC 133 and OSC 7 natively.

status is-interactive; or return 0
set -q __rift_integration_loaded; and return 0
set -g __rift_integration_loaded 1

set -l __rift_major (string match -r '^\d+' -- $version)
if test -n "$__rift_major"; and test "$__rift_major" -ge 4
    return 0
end

function __rift_osc7 --on-variable PWD
    printf '\e]7;file://%s%s\a' (hostname) (string escape --style=url -- $PWD)
end

function __rift_preexec --on-event fish_preexec
    printf '\e]133;C\a'
end

function __rift_postexec --on-event fish_postexec
    set -l s $status
    printf '\e]133;D;%s\a' $s
end

# Fires before each prompt is drawn. On the first one config.fish has fully
# loaded, so wrap whatever fish_prompt the user ended up with to append B.
function __rift_prompt --on-event fish_prompt
    printf '\e]133;A\a'
    __rift_osc7
    if not functions -q __rift_orig_fish_prompt
        functions -c fish_prompt __rift_orig_fish_prompt
        function fish_prompt
            __rift_orig_fish_prompt
            printf '\e]133;B\a'
        end
    end
end
