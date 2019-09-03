import * as React from 'react'
import  TitleBar  from './bits/titlebar'
import  EnhancedTable  from './bits/media-table'

interface AppInterface {}


class App extends React.Component<AppInterface> {
  render() {
    return (
      <div>
        <TitleBar />
        <h1>Hello, Electron!</h1>

        <EnhancedTable />
      </div>
    )
  }
}

export default App
