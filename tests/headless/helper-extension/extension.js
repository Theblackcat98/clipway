// Test-only: lets the headless test drive the shell through
// org.gnome.Shell.Eval. Never install this on a real desktop.
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

export default class ClipwayTestHelper extends Extension {
    enable() {
        global.context.unsafe_mode = true;
    }

    disable() {
        global.context.unsafe_mode = false;
    }
}
