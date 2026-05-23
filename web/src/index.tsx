import ReactDOM from 'react-dom/client'
import { TempsConsole } from './App'

const rootEl = document.getElementById('root')
if (rootEl) {
  const root = ReactDOM.createRoot(rootEl)
  root.render(<TempsConsole />)
}
