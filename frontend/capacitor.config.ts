import type { CapacitorConfig } from '@capacitor/cli'

// The iOS and Android apps wrap the same web build (dist/). Deep links use
// the ferry:// scheme; the web origin stays https://ferry-hacknc.vercel.app.
const config: CapacitorConfig = {
  appId: 'app.ferry.hacknc',
  appName: 'Ferry',
  webDir: 'dist',
  backgroundColor: '#f6fbff',
  ios: {
    contentInset: 'never',
    backgroundColor: '#f6fbff',
  },
  android: {
    backgroundColor: '#f6fbff',
  },
  plugins: {
    SplashScreen: {
      launchShowDuration: 600,
      launchAutoHide: true,
      launchFadeOutDuration: 300,
      backgroundColor: '#f6fbff',
      showSpinner: false,
    },
    StatusBar: {
      style: 'LIGHT',
      overlaysWebView: true,
    },
    Keyboard: {
      resize: 'native',
    },
  },
}

export default config
