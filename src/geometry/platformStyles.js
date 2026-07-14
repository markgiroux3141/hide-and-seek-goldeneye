// Platform style registry — bundles geometry builders + texture scheme name
// per visual style. Adding a new style is just adding an entry here.

import { buildBoxPlatformGeometry, buildBoxStairGeometry } from './platformGeometry.js';
import { buildSimplePlatformGeometry, buildSimpleStairGeometry } from './simplePlatformGeometry.js';

export const PLATFORM_STYLES = {
    default: {
        label: 'Default',
        schemeName: 'facility_white_tile',
        buildPlatform: buildBoxPlatformGeometry,
        buildStair: buildBoxStairGeometry,
    },
    simple: {
        label: 'Simple',
        schemeName: 'simple_blue',
        buildPlatform: buildSimplePlatformGeometry,
        buildStair: buildSimpleStairGeometry,
        doubleSided: true,
    },
};

export function getPlatformStyle(name) {
    return PLATFORM_STYLES[name] || PLATFORM_STYLES.default;
}
