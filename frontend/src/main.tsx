import { Component } from 'react';
import type { ReactNode } from 'react';
import { createRoot } from 'react-dom/client';
import { App } from './App';
import './style.css';

class Boundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  state = { failed: false };
  static getDerivedStateFromError() { return { failed: true }; }
  render() {
    return this.state.failed ? <main><h1>Console unavailable</h1><p>Reload to reconnect. No error payload has been recorded.</p></main> : this.props.children;
  }
}

createRoot(document.getElementById('root')!, {
  onCaughtError: () => {}, onUncaughtError: () => {}, onRecoverableError: () => {},
}).render(<Boundary><App/></Boundary>);
