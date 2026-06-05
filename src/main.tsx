// Unregister any stale Service Worker from a previous version.
// Old Service Workers intercept fetch requests and serve cached JS,
// causing the app to show an old UI despite updated server files.
if ('serviceWorker' in navigator) {
  navigator.serviceWorker.getRegistrations().then((regs) => {
    for (const reg of regs) {
      reg.unregister()
    }
  })
}

import { createRoot } from 'react-dom/client'
import './index.css'
import App from './App.tsx'

createRoot(document.getElementById('root')!).render(<App />)
