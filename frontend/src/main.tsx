import { diagnostics } from './diagnostics/store';
import { Component } from 'react';
import type { ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { Offline } from './debug/Offline';
import { App } from './App';
import './style.css';

class Boundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  render() {
    return this.state.failed ? <main><h1>Console unavailable</h1><p>Reload to reconnect. A sanitized render-failure category was recorded; no error payload was retained.</p></main> : this.props.children;
  }
}

window.addEventListener('error', () => diagnostics.exception('uncaught'));
window.addEventListener('unhandledrejection', () => diagnostics.exception('rejection'));

createRoot(document.getElementById('root')!, {
  onCaughtError: () => diagnostics.exception('render'),
  onUncaughtError: () => diagnostics.exception('uncaught'),
  onRecoverableError: () => diagnostics.exception('recoverable'),
}).render(<Boundary>{new URLSearchParams(location.search).get('offline') === '1' ? <Offline/> : <App/>}</Boundary>);
