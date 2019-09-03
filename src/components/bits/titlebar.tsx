import {Titlebar, Color} from 'custom-electron-titlebar';
import React from 'react';
import  './titlebar.css'

new Titlebar({
    backgroundColor:  Color.fromHex('#3c3c3c')
});


export default class TitleBar extends React.Component {
    render() {
        
        return (
            <div className='titlebar-holder'></div>
        )
    }
}