import React from 'react'
import ReactDOM from 'react-dom/client'
import App from './App'

/**
 * Root component that initializes the React application.
 * Wraps the main App component with React.StrictMode for additional checks
 * and warnings in development mode.
 */
ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>
)
